//! 旧几何路径的末格 Tab 只借真实有限 Core 窗口证明表尾；不把局部 Vec 的末行升级为 End。

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableWindowRequest, MarkdownTableWindowStep, MarkdownTableWindowTarget,
    VerifiedMarkdownTableWindow,
};
use omni_markdown_core::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppendPhase {
    ResolveNext,
    ResolveLast,
    Committing,
    Reopen,
}

/// 一个 Tab 的唯一业务槽；Core scratch 只在 Resolve 内存在，正文和原生图均只有弱身份。
pub(super) struct NativeMarkdownPendingTableAppend {
    identity: WeakMarkdownDocument,
    key: Cell<NativeMarkdownTableLayoutRequestKey>,
    input_epoch: u64,
    fold_revision: u64,
    header_byte_start: usize,
    original: NativeMarkdownTableCellEditorState,
    viewport: std::ops::Range<usize>,
    phase: Cell<AppendPhase>,
    core: RefCell<Option<MarkdownTableWindowRequest>>,
    committed: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    probe_receipt: Option<Rc<NativeMarkdownTableAppendReceipt>>,
}

/// 诊断只报告原动作的真实终点；取消或失败须明确此前是否已经原子提交正文。
#[cfg(feature = "native-markdown-runtime-probe")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownTableAppendOutcome {
    Activated,
    Cancelled { committed: bool },
    Failed { committed: bool },
}

/// 只含 Weak、标量和函数指针；不能持有 Native、正文 root 或 Core 请求 scratch。
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) struct NativeMarkdownTableAppendReceipt {
    document: WeakMarkdownDocument,
    generation: u64,
    coordinate: NativeMarkdownTableCellCoordinate,
    source: std::ops::Range<usize>,
    before_input_epoch: u64,
    accepted_input_epoch: u64,
    fold_revision: u64,
    outcome: Cell<Option<NativeMarkdownTableAppendOutcome>>,
    callback: Cell<Option<fn(NativeMarkdownTableAppendOutcome)>>,
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl NativeMarkdownTableAppendReceipt {
    pub(super) fn outcome(&self) -> Option<NativeMarkdownTableAppendOutcome> {
        self.outcome.get()
    }

    /// 原提交前绑定的代次，供独立 driver 验证一次编辑恰好推进一代。
    pub(super) fn original_generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn original_coordinate(&self) -> NativeMarkdownTableCellCoordinate {
        self.coordinate
    }

    fn finish(&self, outcome: NativeMarkdownTableAppendOutcome) {
        if self.outcome.get().is_some() {
            return; // 一次终点不能被后续槽移除覆盖，重复撤销也不能二次投递。
        }
        self.outcome.set(Some(outcome));
        if let Some(callback) = self.callback.take() {
            // 仅函数指针与 Copy 结果跨队列；回调合同只 post 下一个事件，不采新权限或运行断言。
            // exec_async 只负责排队，不保证外层 Native 栈已退出；driver 须守原票并仅 post 独立事件。
            enqueue_native_markdown_main_task(move || callback(outcome));
        }
    }

    fn matches_request(
        &self,
        document: &WeakMarkdownDocument,
        key: NativeMarkdownTableLayoutRequestKey,
        epoch: u64,
        fold_revision: u64,
        original: &NativeMarkdownTableCellEditorState,
    ) -> bool {
        self.outcome.get().is_none()
            && std::rc::Weak::ptr_eq(&self.document, document)
            && self.generation == key.document_generation
            && self.accepted_input_epoch == epoch
            && self.fold_revision == fold_revision
            && self.coordinate == original.coordinate
            && self.source == original.source_utf16_range
    }
}

/// 请求构造失败也有明确失败终点；移入真实请求后该局部不再拥有完成权。
#[cfg(feature = "native-markdown-runtime-probe")]
struct ProbeReceiptAdmission<'a> {
    view: &'a PieceBackedMarkdownTextView,
    receipt: Option<Rc<NativeMarkdownTableAppendReceipt>>,
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Drop for ProbeReceiptAdmission<'_> {
    fn drop(&mut self) {
        if let Some(receipt) = self.receipt.take() {
            let current =
                self.view.ivars().table_input_intent_epoch.get() == receipt.accepted_input_epoch
                    && self
                        .view
                        .ivars()
                        .folds
                        .read_is_current(receipt.fold_revision)
                    && receipt.document.upgrade().is_some_and(|document| {
                        document.borrow().generation() == receipt.generation
                    });
            receipt.finish(if current {
                NativeMarkdownTableAppendOutcome::Failed { committed: false }
            } else {
                NativeMarkdownTableAppendOutcome::Cancelled { committed: false }
            });
        }
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Drop for NativeMarkdownPendingTableAppend {
    fn drop(&mut self) {
        // 发布前失败、accepted release 和普通取消均不能遗留一个永远 Pending 的原收据。
        self.finish_probe(NativeMarkdownTableAppendOutcome::Cancelled {
            committed: self.probe_was_committed(),
        });
    }
}

/// Complete 才沿原 executor 刷新几何；Progress 不推进输入 epoch，也不要求重绘。
pub(super) enum NativeMarkdownTableAppendPoll {
    None,
    Progress,
    Blocked,
    Reopen,
}

impl NativeMarkdownPendingTableAppend {
    fn commit(&self) -> Option<Rc<NativeMarkdownCommittedSelection>> {
        self.committed.borrow().as_ref().map(Rc::clone)
    }

    fn target(&self) -> NativeMarkdownTableCellCoordinate {
        NativeMarkdownTableCellCoordinate {
            row_index: self.original.coordinate.row_index + 1,
            column_index: 0,
            ..self.original.coordinate
        }
    }

    fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        let key = self.key.get();
        if !view.owns_table_append(self)
            || view.ivars().textkit_graph_teardown.get()
            || !storage.ivars().accepts_edits.get()
            || view.ivars().table_input_intent_epoch.get() != self.input_epoch
            || storage.presentation_mode() != MarkdownPresentationMode::InlinePreview
            || storage.ivars().table_projection_width_key.get() != key.text_view_width
            || !std::rc::Weak::ptr_eq(&self.identity, &storage.ivars().document.borrow())
        {
            return false; // 新输入、宽度、模式或关闭均不允许旧 Tab 继续消费本地值。
        }
        let committed = self.commit();
        if committed.as_ref().is_some_and(|receipt| !receipt.current())
            || (committed.is_none() && !view.ivars().folds.read_is_current(self.fold_revision))
        {
            return false; // 提交前守原 fold 票；提交后只认 pre-notify 签发的原提交票。
        }
        self.identity.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == key.document_generation
                && document.utf16_len() == key.document_length
        })
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    fn finish_probe(&self, outcome: NativeMarkdownTableAppendOutcome) {
        if let Some(receipt) = &self.probe_receipt {
            receipt.finish(outcome);
        }
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    fn probe_was_committed(&self) -> bool {
        self.committed.borrow().is_some()
    }

    /// Native claims 释放后继续核原纯提交票，不重新获取原生图或补签。
    #[cfg(feature = "native-markdown-runtime-probe")]
    fn probe_completion_authority_is_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        let key = self.key.get();
        view.owns_table_append(self)
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
            && self.commit().is_some_and(|receipt| receipt.current())
            && self.identity.upgrade().is_some_and(|document| {
                let document = document.borrow();
                document.generation() == key.document_generation
                    && document.utf16_len() == key.document_length
            })
    }

    /// Activated 还要求刚消费的新格几何属于同一批 width/source；缺少几何不能升级为成功。
    #[cfg(feature = "native-markdown-runtime-probe")]
    fn probe_activation_is_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        let key = self.key.get();
        self.probe_completion_authority_is_current(view)
            && view
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    cache.request_key.document_generation == key.document_generation
                        && cache.request_key.document_length == key.document_length
                        && cache.request_key.presentation_mode == key.presentation_mode
                        && cache.request_key.text_view_width == key.text_view_width
                        && std::rc::Weak::ptr_eq(&cache.document_identity, &self.identity)
                })
    }
}

impl PieceBackedMarkdownTextView {
    fn table_append(&self) -> Option<Rc<NativeMarkdownPendingTableAppend>> {
        self.ivars().table_append.borrow().as_ref().map(Rc::clone)
    }

    fn owns_table_append(&self, request: &NativeMarkdownPendingTableAppend) -> bool {
        self.ivars()
            .table_append
            .borrow()
            .as_ref()
            .is_some_and(|slot| std::ptr::eq(&**slot, request))
    }

    fn discard_table_append(&self, request: &NativeMarkdownPendingTableAppend) {
        let previous = {
            let mut slot = self.ivars().table_append.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|value| std::ptr::eq(&**value, request))
            {
                slot.take()
            } else {
                None
            }
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        if previous.is_some() {
            request.finish_probe(NativeMarkdownTableAppendOutcome::Cancelled {
                committed: request.probe_was_committed(),
            }); // 已 Activated/Failed 的原收据不会被此正常移除覆盖。
        }
        drop(previous); // Core scratch、Weak 和纯 receipt 最后一个 owner 都在 RefMut 结束后释放。
    }

    /// 取消只取当前唯一槽，不清理字段内容；实际输入仍由原 field editor 保留。
    pub(super) fn cancel_pending_table_append(&self) {
        let previous = { self.ivars().table_append.borrow_mut().take() };
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(request) = &previous {
            request.finish_probe(NativeMarkdownTableAppendOutcome::Cancelled {
                committed: request.probe_was_committed(),
            });
        }
        drop(previous);
        #[cfg(feature = "native-markdown-runtime-probe")]
        self.cancel_predeclared_table_append_receipt(false);
    }

    /// 必须与随后唯一生产 navigate 调用处于同一同步栈，中间不得调用 Native 或投递其他动作。
    /// 原导航已固定会 advance 一次输入 epoch；本收据只接受这一代，不能被后继 Tab 借用。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn probe_prepare_table_append_receipt(
        &self,
        document: &SharedMarkdownDocument,
        callback: fn(NativeMarkdownTableAppendOutcome),
    ) -> Result<Rc<NativeMarkdownTableAppendReceipt>, String> {
        let before_input_epoch = self.ivars().table_input_intent_epoch.get();
        let fold_revision = self.ivars().folds.revision();
        if !self.ivars().folds.read_is_current(fold_revision)
            || !self.ivars().folds.matches_document(document)
            || self.ivars().textkit_graph_teardown.get()
            || self.table_append_pending()
            || self.ivars().table_append_probe_receipt.borrow().is_some()
        {
            return Err("末格验收的原动作权限不可用".into());
        }
        let (coordinate, source) = self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .map(|active| (active.coordinate, active.source_utf16_range.clone()))
            .ok_or("末格验收缺少活动草稿")?;
        let receipt = Rc::new(NativeMarkdownTableAppendReceipt {
            document: Rc::downgrade(document),
            generation: document.borrow().generation(),
            coordinate,
            source,
            before_input_epoch,
            accepted_input_epoch: before_input_epoch
                .checked_add(1)
                .ok_or("表格输入代次已达上限")?,
            fold_revision,
            outcome: Cell::new(None),
            callback: Cell::new(Some(callback)),
        });
        self.ivars()
            .table_append_probe_receipt
            .replace(Some(Rc::clone(&receipt)));
        Ok(receipt)
    }

    /// 首次 navigate 的现有撤权调用只消费预声明的一次 advance；其余新输入或 close 都取消。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn cancel_predeclared_table_append_receipt(&self, closing: bool) {
        let previous = {
            let mut slot = self.ivars().table_append_probe_receipt.borrow_mut();
            if slot.as_ref().is_some_and(|receipt| {
                closing || self.ivars().table_input_intent_epoch.get() != receipt.before_input_epoch
            }) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(receipt) = previous {
            receipt.finish(NativeMarkdownTableAppendOutcome::Cancelled { committed: false });
        }
    }

    pub(super) fn table_append_pending(&self) -> bool {
        self.ivars().table_append.borrow().is_some()
    }

    /// 激活的既有逐 Native 门禁也必须消费原提交票，不能在通知后重领新的选择权限。
    pub(super) fn table_append_commit_is_current(&self, epoch: u64) -> bool {
        self.table_append().is_none_or(|request| {
            request.input_epoch != epoch || request.commit().is_none_or(|receipt| receipt.current())
        })
    }

    /// 只有旧导航确认位于逻辑末列时进入新流程；真实“下一行不存在”仍由 Core 判定。
    pub(super) fn request_terminal_table_append(
        &self,
        original: NativeMarkdownTableCellEditorState,
        authority: &NativeMarkdownTableCellAuthority,
        epoch: u64,
        fold_revision: u64,
    ) -> Result<(), String> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let mut probe_admission = ProbeReceiptAdmission {
            view: self,
            receipt: self.ivars().table_append_probe_receipt.borrow_mut().take(),
        }; // 在首 Native 前绑定原预声明；任何早退由同一局部失败出口完成。
        let original_coordinate = original.coordinate;
        let original_range = original.source_utf16_range.clone();
        let initial = || {
            self.ivars().table_input_intent_epoch.get() == epoch
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().folds.read_is_current(fold_revision)
                && self
                    .ivars()
                    .active_table_cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|active| {
                        active.coordinate == original_coordinate
                            && active.source_utf16_range == original_range
                    })
        };
        let document = authority.document.upgrade().ok_or("表格所属文档已关闭")?;
        let (columns, viewport) = {
            let document = document.borrow();
            if authority.source_range(&document, original_coordinate)? != original_range {
                return Err("原末格源范围已经改变，草稿将保持打开".into());
            }
            let window = authority
                .window
                .presentation_for(&document)
                .map_err(|_| "原表格窗口已失效")?;
            let row = window
                .row(original_coordinate.row_index)
                .ok_or("当前真实表格行已改变")?;
            (window.column_count, row.utf16_range.clone())
        };
        if !initial()
            || original.coordinate.column_index.checked_add(1) != Some(columns)
            || original.coordinate.row_index.checked_add(1).is_none()
        {
            return Err("表格末格输入已经改变，草稿将保持打开".into());
        }
        let storage =
            native_markdown_table_intent_step(&initial, || self.ivars().text_storage.load())
                .map_err(|_| "表格输入已撤销")?
                .ok_or("表格所属文档已关闭")?;
        let key =
            native_markdown_table_intent_step(&initial, || self.current_table_layout_request_key())
                .map_err(|_| "表格输入已撤销")?
                .ok_or("当前表格几何不可用")?;
        if !std::rc::Weak::ptr_eq(&authority.document, &storage.ivars().document.borrow()) {
            return Err("表格窗口不属于原文档".into());
        }
        let header_byte_start = authority.header_byte_start;
        let core = {
            let document = document.borrow();
            let selection = document.selection();
            document
                .table_window_request(
                    MarkdownTableWindowTarget::RequiredRow {
                        header_byte_start,
                        row_ordinal: original.coordinate.row_index + 1,
                    },
                    viewport.clone(),
                    fold_revision,
                    selection.anchor_utf16.min(selection.head_utf16)
                        ..selection.anchor_utf16.max(selection.head_utf16),
                )
                .map_err(|_| "无法准备真实表尾验证，草稿将保持打开")?
        };
        let identity = Rc::downgrade(&document);
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(receipt) = &probe_admission.receipt
            && !receipt.matches_request(&identity, key, epoch, fold_revision, &original)
        {
            receipt.finish(NativeMarkdownTableAppendOutcome::Cancelled { committed: false });
            probe_admission.receipt = None; // 不匹配的预声明不能附到另一真实 Tab 上。
        }
        let request = Rc::new(NativeMarkdownPendingTableAppend {
            identity,
            key: Cell::new(key),
            input_epoch: epoch,
            fold_revision,
            header_byte_start,
            original,
            viewport,
            phase: Cell::new(AppendPhase::ResolveNext),
            core: RefCell::new(Some(core)),
            committed: RefCell::new(None),
            #[cfg(feature = "native-markdown-runtime-probe")]
            probe_receipt: probe_admission.receipt.take(),
        });
        let admitted = initial()
            && self.table_input_intent_is_current(&storage, &request.identity, key, epoch);
        drop(storage); // 最后 Native 临时 claim 先释放；析构若撤权，下面不能再发布旧请求。
        drop(document);
        if !admitted
            || !initial()
            || !request.identity.upgrade().is_some_and(|document| {
                let document = document.borrow();
                document.generation() == key.document_generation
                    && document.utf16_len() == key.document_length
            })
        {
            return Err("表格末格输入已撤销，草稿将保持打开".into());
        }
        let previous = self.ivars().table_append.replace(Some(Rc::clone(&request)));
        drop(previous);
        self.schedule_visible_table_layout_update();
        Ok(())
    }

    /// 纯 Core 每轮只推进一次；真实 End 先转成末行请求，下轮才继续，不在回调中 while 到 Ready。
    pub(super) fn advance_pending_table_append(&self) -> NativeMarkdownTableAppendPoll {
        let Some(request) = self.table_append() else {
            return NativeMarkdownTableAppendPoll::None;
        };
        if request.phase.get() == AppendPhase::Committing {
            return NativeMarkdownTableAppendPoll::Blocked; // 同步字符通知内的嵌套 executor 不得二次追加。
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            self.discard_table_append(&request);
            return NativeMarkdownTableAppendPoll::None;
        };
        if !request.current(self, &storage) {
            self.discard_table_append(&request);
            return NativeMarkdownTableAppendPoll::None;
        }
        if request.phase.get() == AppendPhase::Reopen {
            return NativeMarkdownTableAppendPoll::Reopen;
        }
        let result = self.advance_table_append_core(&request, &storage);
        match result {
            Ok(None) => NativeMarkdownTableAppendPoll::Progress, // Pending 不发布属性或空几何。
            Ok(Some(window)) => {
                request.phase.set(AppendPhase::Committing);
                match self.commit_terminal_table_append(&request, &storage, &window) {
                    Ok(()) if request.current(self, &storage) => {
                        request.phase.set(AppendPhase::Reopen);
                        let committed = request
                            .commit()
                            .ok_or_else(|| "表格追加缺少原提交收据".to_owned());
                        let started = committed.and_then(|committed| {
                            self.start_required_table_cell(
                                request.target(),
                                super::table_editor::NativeMarkdownTableCellAction::Activate,
                                super::table_editor::NativeMarkdownTableCellIntent {
                                    input_epoch: request.input_epoch,
                                    fold_revision: request.fold_revision,
                                    committed: Some(committed),
                                },
                                Some(request.header_byte_start),
                                &|| request.current(self, &storage),
                            )
                        });
                        if let Err(error) = started {
                            self.discard_table_append(&request);
                            present_native_markdown_error(&error);
                            return NativeMarkdownTableAppendPoll::None;
                        }
                        NativeMarkdownTableAppendPoll::Progress // 新行必须异步取得同g1 Ready，不能立即借旧矩阵激活。
                    }
                    Ok(()) => {
                        self.discard_table_append(&request);
                        NativeMarkdownTableAppendPoll::None // 已提交后的新输入拥有界面，不回滚或清草稿。
                    }
                    Err(error) => {
                        let report = request.current(self, &storage);
                        drop(storage); // 收据排队前释放本分支的临时 Native claim，不跨其析构发布。
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        request.finish_probe(if report {
                            NativeMarkdownTableAppendOutcome::Failed {
                                committed: request.probe_was_committed(),
                            }
                        } else {
                            NativeMarkdownTableAppendOutcome::Cancelled {
                                committed: request.probe_was_committed(),
                            }
                        });
                        self.discard_table_append(&request);
                        if report {
                            present_native_markdown_error(&error);
                        }
                        NativeMarkdownTableAppendPoll::None
                    }
                }
            }
            Err(error) => {
                let report = request.current(self, &storage);
                drop(storage); // 原失败首因和 report 保留；回调不在持 storage 的栈内排队。
                #[cfg(feature = "native-markdown-runtime-probe")]
                request.finish_probe(if report {
                    NativeMarkdownTableAppendOutcome::Failed {
                        committed: request.probe_was_committed(),
                    }
                } else {
                    NativeMarkdownTableAppendOutcome::Cancelled {
                        committed: request.probe_was_committed(),
                    }
                });
                self.discard_table_append(&request);
                if report {
                    present_native_markdown_error(&error);
                }
                NativeMarkdownTableAppendPoll::None
            }
        }
    }

    fn advance_table_append_core(
        &self,
        request: &NativeMarkdownPendingTableAppend,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> Result<Option<VerifiedMarkdownTableWindow>, String> {
        let document = storage.document().ok_or("表格所属文档已关闭")?;
        let document = document.borrow();
        let mut slot = request.core.borrow_mut();
        let core = slot.as_mut().ok_or("表格末行请求已结束")?;
        match core
            .advance(&document, false, 4096)
            .map_err(|_| "表格末行验证未完成或超过预算，草稿将保持打开")?
        {
            MarkdownTableWindowStep::Progress { .. } => Ok(None),
            MarkdownTableWindowStep::TableEndReached { .. } => {
                let end = core
                    .required_row_end_for(&document)
                    .map_err(|_| "表格末尾证明已失效")?
                    .ok_or("表格末尾尚未得到验证")?;
                if request.phase.get() != AppendPhase::ResolveNext
                    || end.header_byte_start() != request.header_byte_start
                    || end.last_body_row_ordinal().unwrap_or(0)
                        != request.original.coordinate.row_index
                {
                    return Err("当前格不是已验证的整表末行，草稿将保持打开".into());
                }
                let selection = document.selection();
                let next = document
                    .table_window_request(
                        MarkdownTableWindowTarget::RequiredRow {
                            header_byte_start: request.header_byte_start,
                            row_ordinal: request.original.coordinate.row_index,
                        },
                        request.viewport.clone(),
                        request.fold_revision,
                        selection.anchor_utf16.min(selection.head_utf16)
                            ..selection.anchor_utf16.max(selection.head_utf16),
                    )
                    .map_err(|_| "无法取得已验证的末行窗口")?;
                let previous = slot.replace(next);
                request.phase.set(AppendPhase::ResolveLast);
                drop(slot);
                drop(previous); // 结构 End 不授予编辑；旧请求释放后下轮取得真实完整语义窗口。
                Ok(None)
            }
            MarkdownTableWindowStep::Ready { .. } => {
                if request.phase.get() != AppendPhase::ResolveLast {
                    return Err("表格仍有下一行，本次未追加；草稿将保持打开".into());
                }
                let window = core
                    .take_ready_for(&document)
                    .map_err(|_| "表格末行窗口已失效")?
                    .ok_or("表格末行窗口尚未就绪")?;
                validate_append_window(
                    &document,
                    &window,
                    request.header_byte_start,
                    &request.original,
                )?;
                let previous = slot.take();
                drop(slot);
                drop(previous); // Ready 移交后不留下 Parser、源复制 scratch 或重复消费入口。
                Ok(Some(window))
            }
            MarkdownTableWindowStep::NotTable { .. } => {
                Err("原位置已不是可验证的表格，草稿将保持打开".into())
            }
        }
    }

    /// 单事务开始前完成真实字段确认；只有具名同步通知可保留本 Tab 权限。
    fn commit_terminal_table_append(
        &self,
        request: &NativeMarkdownPendingTableAppend,
        storage: &PieceBackedMarkdownTextStorage,
        window: &VerifiedMarkdownTableWindow,
    ) -> Result<(), String> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let pending_tail = RefCell::new(None); // 原 append owner 必须最后释放，不能早于字段 Native claims。
        let selection_capture = self
            .ivars()
            .folds
            .capture_selection()
            .map_err(|error| error.to_string())?;
        let current = || {
            request.current(self, storage)
                && (request.committed.borrow().is_some() || selection_capture.current())
        };
        let expired = || "表格输入已变化，草稿将保持打开".to_owned();
        let field =
            native_markdown_table_intent_step(&current, || self.existing_table_cell_editor())
                .map_err(|_| expired())?
                .ok_or_else(expired)?;
        let actual = native_markdown_table_intent_step(&current, || field.currentEditor())
            .map_err(|_| expired())?
            .and_then(|editor| editor.downcast::<NSTextView>().ok());
        if let Some(actual) = actual.as_ref() {
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**actual);
            if native_markdown_table_intent_step(&current, || input.hasMarkedText())
                .map_err(|_| expired())?
            {
                self.expected_table_field_operation(
                    &field,
                    actual,
                    NativeMarkdownTableFieldOperation::Unmark,
                    &current,
                    || input.unmarkText(),
                )?;
            }
            if native_markdown_table_intent_step(&current, || input.hasMarkedText())
                .map_err(|_| expired())?
            {
                return Err("表格输入尚未完成，草稿将保持打开".into());
            }
            self.expected_table_field_operation(
                &field,
                actual,
                NativeMarkdownTableFieldOperation::Validate,
                &current,
                || field.validateEditing(),
            )?;
        } else {
            native_markdown_table_intent_step(&current, || field.validateEditing())
                .map_err(|_| expired())?;
        }
        let replacement = (|| {
            let value = native_markdown_table_intent_step(&current, || field.stringValue())
                .map_err(|_| expired())?;
            let units = native_markdown_table_intent_step(&current, || value.len_utf16())
                .map_err(|_| expired())?;
            if units > 64 * 1024 {
                return Err("表格单元格输入超过预算，草稿将保持打开".to_owned());
            }
            autoreleasepool(|pool| {
                // SAFETY: 单格长度已准入；pool 内源值保持活跃，仅复制有限 UTF-8 后返回。
                let text =
                    native_markdown_table_intent_step(&current, || unsafe { value.to_str(pool) })
                        .map_err(|_| expired())?;
                Ok(native_markdown_sanitize_table_cell_input(text))
            })
        })()?;
        drop(actual); // 实际 editor/string 临时 claim 释放后再核原票，不能借析构后的新输入。
        if !current() {
            return Err(expired());
        }
        let document = storage.document().ok_or_else(expired)?;
        let (plan, baseline) = {
            let document = document.borrow();
            validate_append_window(
                &document,
                window,
                request.header_byte_start,
                &request.original,
            )?;
            let cell = document
                .plan_verified_table_cell_edit(
                    window,
                    request.original.coordinate.row_index,
                    request.original.coordinate.column_index,
                    &replacement,
                )
                .map_err(|_| "无法准备表格单元格提交")?;
            let (range, text) = cell
                .replacement_for(&document)
                .map_err(|_| "表格单元格源已改变")?;
            let end = range
                .start
                .checked_add(text.encode_utf16().count())
                .ok_or("表格单元格位置溢出")?;
            let start = end
                .checked_sub(replacement.encode_utf16().count())
                .ok_or("表格单元格位置溢出")?;
            let plan = document
                .plan_verified_table_row_append(
                    window,
                    request.original.coordinate.row_index,
                    request.original.coordinate.column_index,
                    &replacement,
                )
                .map_err(|_| "无法准备已验证表尾的追加行")?;
            (plan, start..end)
        };
        let observed = native_markdown_table_intent_step(&current, || {
            self.native_ordered_selection_state_for_capture(&selection_capture)
        })
        .map_err(|_| expired())?
        .map_err(|error| error.to_string())?;
        let selection_origin = match observed {
            NativeMarkdownOrderedSelection::Single(_) => selection_capture.clone(),
            NativeMarkdownOrderedSelection::Multiple => {
                // 原末行窗口已验证该格源；显式表格动作可选其起点，不能从 Multi 猜一个原主 head。
                let caret = request.original.source_utf16_range.start;
                let target = selection_capture
                    .clone()
                    .finish(MarkdownTextSelection::caret(caret), true)
                    .map_err(|error| error.to_string())?;
                let receipt = storage
                    .update_active_selection_observed(target, |receipt| {
                        // 同 g/len 的唯一纯选择后果直接交原请求；不修改输入/模式/宽度/slot 权限。
                        request.committed.replace(Some(Rc::clone(receipt)));
                    })
                    .ok_or_else(expired)?;
                if !current() || !self.apply_committed_selection_if(&receipt, current) {
                    return Err("末格选择已收拢，但原追加动作已撤销；草稿仍保留".into());
                }
                receipt
                    .captured_selection()
                    .map_err(|error| error.to_string())?
                    .native_capture()
                    .clone()
            }
        };
        let captured = self.capture_ordered_native_before(selection_origin, storage, &current)?;
        let (_, committed, pending) = storage.apply_planned_table_replacement(
            &captured,
            |document| {
                plan.replacement_for(document)
                    .map(|(range, text)| (range, text, None)) // 末格追加保持原真实插入尾 caret。
                    .map_err(|_| "表格提交源已改变".into())
            },
            &current,
            |edit, committed| {
                // 纯发布位于 Core 成功之后、任何 Native 通知之前；后继输入保留新基线，不能二次追加。
                let mut key = request.key.get();
                let generation = key.document_generation.checked_add(1);
                let length = key
                    .document_length
                    .checked_add_signed(edit.change_in_utf16_length);
                if self.owns_table_append(request)
                    && self.ivars().table_input_intent_epoch.get() == request.input_epoch
                    && committed.current()
                    && let (Some(generation), Some(length)) = (generation, length)
                {
                    key.document_generation = generation;
                    key.document_length = length;
                    request.key.set(key);
                    let previous = request.committed.replace(Some(Rc::clone(committed)));
                    drop(previous);
                    let mut active = self.ivars().active_table_cell.borrow_mut();
                    if let Some(active) = active
                        .as_mut()
                        .filter(|active| active.coordinate == request.original.coordinate)
                    {
                        active.source_utf16_range = baseline.clone();
                        active.original_source.clone_from(&replacement);
                    }
                }
            },
        )?;
        drop(pending_tail.replace(pending));
        if !current() {
            return Ok(());
        } // 真实提交不能撤销；新的用户意图保留字段和草稿。
        native_markdown_table_intent_step(&current, || field.abortEditing())
            .map_err(|_| expired())?;
        native_markdown_table_intent_step(&current, || field.setHidden(true))
            .map_err(|_| expired())?;
        let previous = self.ivars().active_table_cell.borrow_mut().take();
        drop(previous);
        native_markdown_table_intent_step(&current, || field.setStringValue(ns_string!("")))
            .map_err(|_| expired())?;
        if !self.apply_committed_selection_if(&committed, current) {
            return Err("表尾已追加，但正文选择的原安装动作已撤销".into());
        }
        Ok(())
    }

    /// 原有一次几何准备后只消费原 Reopen；没有真实首格就明确失败，不重新请求或循环激活。
    pub(super) fn finish_pending_table_append(&self) {
        let Some(request) = self.table_append() else {
            return;
        };
        if request.phase.get() != AppendPhase::Reopen {
            return;
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            self.discard_table_append(&request);
            return;
        };
        let current = request.current(self, &storage);
        let target = request.target();
        if current && self.ivars().pending_table_cell_activation.get() == Some(target) {
            return; // 同原append票的RequiredRow尚在推进或准备真实矩阵，未到终点。
        }
        let active = self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .map(|state| state.coordinate);
        let succeeded = current
            && active == Some(target)
            && self.ivars().pending_table_cell_activation.get().is_none();
        drop(storage); // 原激活使用的临时 Native claim 先释放，收据发布不跨越其析构。
        #[cfg(feature = "native-markdown-runtime-probe")]
        request.finish_probe(if succeeded && request.probe_activation_is_current(self) {
            NativeMarkdownTableAppendOutcome::Activated
        } else if current && request.probe_completion_authority_is_current(self) {
            NativeMarkdownTableAppendOutcome::Failed {
                committed: request.probe_was_committed(),
            }
        } else {
            NativeMarkdownTableAppendOutcome::Cancelled {
                committed: request.probe_was_committed(),
            }
        });
        self.discard_table_append(&request);
        if current && !succeeded {
            if self.ivars().pending_table_cell_activation.get() == Some(target) {
                self.ivars().pending_table_cell_activation.set(None);
            }
            present_native_markdown_error("新行已追加，但首格布局尚不可用；原文与撤销历史均已保留");
        }
    }
}

/// 旧完整快照只提供动作起点；最终成员、末行、逻辑列和 cell 源范围均由 opaque 窗口证明。
fn validate_append_window(
    document: &MarkdownDocumentBuffer,
    window: &VerifiedMarkdownTableWindow,
    header_byte_start: usize,
    original: &NativeMarkdownTableCellEditorState,
) -> Result<(), String> {
    let projection = window
        .presentation_for(document)
        .map_err(|_| "表格窗口源已变化")?;
    let end = window
        .table_end_for(document)
        .map_err(|_| "表格末尾源已变化")?
        .ok_or("表格末尾尚未验证")?;
    let coordinate = original.coordinate;
    let cell = projection
        .cell(coordinate.row_index, coordinate.column_index)
        .ok_or("当前逻辑表格单元格不存在")?;
    let same_source = match &cell.source {
        MarkdownTableWindowCellSource::Present(range) => *range == original.source_utf16_range,
        MarkdownTableWindowCellSource::ImplicitEmpty {
            insertion_anchor_utf16,
        } => original.source_utf16_range == (*insertion_anchor_utf16..*insertion_anchor_utf16),
    };
    if !same_source
        || end.header_byte_start() != header_byte_start
        || projection.header_byte_range.start != header_byte_start
        || end.last_body_row_ordinal().unwrap_or(0) != coordinate.row_index
        || coordinate.column_index.checked_add(1) != Some(projection.column_count)
    {
        return Err("当前格与真实表尾不一致，草稿将保持打开".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        // 使用真实 mmap/piece root，让不同文档同长度同代次也必须被 opaque 权限拒绝。
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("terminal-table-tab.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn terminal_window(
        document: &MarkdownDocumentBuffer,
        row: usize,
    ) -> VerifiedMarkdownTableWindow {
        let mut request = document
            .table_window_request(
                MarkdownTableWindowTarget::RequiredRow {
                    header_byte_start: 0,
                    row_ordinal: row,
                },
                0..0,
                17,
                usize::MAX..usize::MAX,
            )
            .unwrap();
        // 测试驱动有显式总步数上限；产品 executor 不使用这个循环，也不一次扫描到 Ready。
        for _ in 0..100_000 {
            match request.advance(document, false, 4096).unwrap() {
                MarkdownTableWindowStep::Progress { .. } => {}
                MarkdownTableWindowStep::Ready { .. } => {
                    return request.take_ready_for(document).unwrap().unwrap();
                }
                other => panic!("真实末行未得到语义窗口: {other:?}"),
            }
        }
        panic!("有限测试请求没有结束");
    }

    fn original(
        document: &MarkdownDocumentBuffer,
        window: &VerifiedMarkdownTableWindow,
        row: usize,
    ) -> NativeMarkdownTableCellEditorState {
        let projection = window.presentation_for(document).unwrap();
        let column = projection.column_count - 1;
        let source_utf16_range = match &projection.cell(row, column).unwrap().source {
            MarkdownTableWindowCellSource::Present(range) => range.clone(),
            MarkdownTableWindowCellSource::ImplicitEmpty {
                insertion_anchor_utf16,
            } => *insertion_anchor_utf16..*insertion_anchor_utf16,
        };
        NativeMarkdownTableCellEditorState {
            authority: None,
            coordinate: NativeMarkdownTableCellCoordinate {
                table_utf16_start: 0,
                row_index: row,
                column_index: column,
            },
            source_utf16_range,
            frame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 24.0)),
            original_source: String::new(),
            field_selection: None,
            pointer: None,
        }
    }

    /// 真尾行必须同时匹配逻辑末列和原实际源范围；同坐标并不足以授权旧草稿。
    #[test]
    fn terminal_witness_rejects_changed_source_or_nonterminal_column() {
        let (_directory, document) = fixture("| A | B |\n| --- | --- |\n| one | two |\n");
        let window = terminal_window(&document, 1);
        let mut active = original(&document, &window, 1);
        assert!(validate_append_window(&document, &window, 0, &active).is_ok());
        active.source_utf16_range.start += 1;
        assert!(validate_append_window(&document, &window, 0, &active).is_err());
        active = original(&document, &window, 1);
        active.coordinate.column_index = 0;
        assert!(validate_append_window(&document, &window, 0, &active).is_err());
    }

    /// 不完整末格用真实 ImplicitEmpty witness；一次计划包含补格、草稿和新行，Undo 恢复原字节。
    #[test]
    fn implicit_terminal_cell_commits_draft_and_append_as_one_undo() {
        let source = "| A | B |\r\n| --- | --- |\r\n| one |\r\n";
        let (_directory, mut document) = fixture(source);
        let window = terminal_window(&document, 1);
        let active = original(&document, &window, 1);
        assert!(active.source_utf16_range.is_empty());
        validate_append_window(&document, &window, 0, &active).unwrap();
        let plan = document
            .plan_verified_table_row_append(&window, 1, 1, "draft")
            .unwrap();
        let (range, replacement) = plan.replacement_for(&document).unwrap();
        let range = range.clone();
        let replacement = replacement.to_owned();
        document
            .replace_utf16_range(range, &replacement, MarkdownTextSelection::caret(0))
            .unwrap();
        assert!(document.undo().is_some());
        assert!(!document.can_undo());
        let mut units = vec![0; document.utf16_len()];
        document.copy_utf16_code_units(0, &mut units).unwrap();
        assert_eq!(String::from_utf16(&units).unwrap(), source);
        assert!(plan.replacement_for(&document).is_err());
    }

    /// H/D-only 也必须是真 End；来自同长、同代次另一文档的窗口没有任何写权限。
    #[test]
    fn header_only_and_other_document_witness_remain_distinct() {
        let source = "| A |\n| --- |";
        let (_first_dir, first) = fixture(source);
        let (_second_dir, second) = fixture(source);
        let window = terminal_window(&first, 0);
        let active = original(&first, &window, 0);
        assert!(validate_append_window(&first, &window, 0, &active).is_ok());
        assert!(validate_append_window(&second, &window, 0, &active).is_err());
        assert!(validate_append_window(&first, &window, 1, &active).is_err());
    }

    /// 失败已签发就不能被正常槽移除或迟到成功覆盖；committed 事实也不得丢失。
    #[cfg(feature = "native-markdown-runtime-probe")]
    #[test]
    fn terminal_receipt_keeps_first_outcome_and_commit_fact() {
        let coordinate = NativeMarkdownTableCellCoordinate {
            table_utf16_start: 0,
            row_index: 1,
            column_index: 1,
        };
        let receipt = NativeMarkdownTableAppendReceipt {
            document: WeakMarkdownDocument::new(),
            generation: 3,
            coordinate,
            source: 0..0,
            before_input_epoch: 4,
            accepted_input_epoch: 5,
            fold_revision: 6,
            outcome: Cell::new(None),
            callback: Cell::new(None),
        };
        assert_eq!(receipt.outcome(), None);
        assert_eq!(receipt.original_generation(), 3);
        assert_eq!(receipt.original_coordinate(), coordinate);
        let failure = NativeMarkdownTableAppendOutcome::Failed { committed: true };
        receipt.finish(failure);
        receipt.finish(NativeMarkdownTableAppendOutcome::Activated);
        receipt.finish(NativeMarkdownTableAppendOutcome::Cancelled { committed: false });
        assert_eq!(receipt.outcome(), Some(failure));
    }
}
