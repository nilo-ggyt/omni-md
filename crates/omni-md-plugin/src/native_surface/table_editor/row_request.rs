//! 一个显式表格命令的有限源权限；只有原executor推进，绘制与命中只消费Ready。

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableWindowRequest, MarkdownTableWindowStep, MarkdownTableWindowTarget,
    VerifiedMarkdownTableWindow,
};
use omni_markdown_core::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowCellSource, navigation::*,
};

/// 保留旧坐标读取接口，但待激活坐标必须同时具有同槽Core请求，不以整数重建权限。
#[derive(Default)]
pub(in crate::native_surface) struct NativeMarkdownPendingTableCell {
    coordinate: Cell<Option<NativeMarkdownTableCellCoordinate>>,
    request: RefCell<Option<Rc<NativeMarkdownTableCellRequest>>>,
}

impl NativeMarkdownPendingTableCell {
    pub(in crate::native_surface) fn get(&self) -> Option<NativeMarkdownTableCellCoordinate> {
        self.coordinate.get()
    }

    pub(in crate::native_surface) fn is_empty(&self) -> bool {
        self.coordinate.get().is_none() && self.request.borrow().is_none()
    }

    /// 关闭、输入和旧清理入口只撤权；裸坐标没有Ready或激活许可。
    pub(in crate::native_surface) fn set(
        &self,
        coordinate: Option<NativeMarkdownTableCellCoordinate>,
    ) {
        self.coordinate.set(coordinate);
        let previous = self.request.borrow_mut().take();
        drop(previous); // Core scratch和纯Weak票在借用外释放，不保活正文或Native图。
    }

    pub(in crate::native_surface) fn request(&self) -> Option<Rc<NativeMarkdownTableCellRequest>> {
        self.request.borrow().as_ref().map(Rc::clone)
    }

    pub(in crate::native_surface) fn owns(&self, request: &NativeMarkdownTableCellRequest) -> bool {
        self.request
            .borrow()
            .as_ref()
            .is_some_and(|value| std::ptr::eq(&**value, request))
    }

    pub(in crate::native_surface) fn publish(&self, request: Rc<NativeMarkdownTableCellRequest>) {
        self.coordinate.set(Some(request.target));
        let previous = self.request.replace(Some(request));
        drop(previous);
    }

    pub(in crate::native_surface) fn clear_if_same(
        &self,
        request: &NativeMarkdownTableCellRequest,
    ) {
        let previous = {
            let mut slot = self.request.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|value| std::ptr::eq(&**value, request))
            {
                self.coordinate.set(None);
                slot.take()
            } else {
                None // 失效局部不能清掉后继动作的请求或坐标。
            }
        };
        drop(previous);
    }
}

/// 活动单格借用同一opaque窗口；Present与ImplicitEmpty不在Native层合并为编辑含义相同的range。
#[derive(Clone)]
pub(in crate::native_surface) struct NativeMarkdownTableCellAuthority {
    pub(in crate::native_surface) document: WeakMarkdownDocument,
    pub(in crate::native_surface) header_byte_start: usize,
    pub(in crate::native_surface) legacy_table_start: usize,
    pub(in crate::native_surface) window: Rc<VerifiedMarkdownTableWindow>,
}

impl NativeMarkdownTableCellAuthority {
    pub(in crate::native_surface) fn source_range(
        &self,
        document: &MarkdownDocumentBuffer,
        coordinate: NativeMarkdownTableCellCoordinate,
    ) -> Result<std::ops::Range<usize>, String> {
        let window = self
            .window
            .presentation_for(document)
            .map_err(|_| "表格窗口源已改变")?;
        if coordinate.table_utf16_start != self.legacy_table_start
            || window.header_byte_range.start != self.header_byte_start
        {
            return Err("表格物理表头与原单元格身份不符".into());
        }
        let cell = window
            .cell(coordinate.row_index, coordinate.column_index)
            .ok_or("当前绝对行或逻辑列不在已验证窗口内")?;
        Ok(match &cell.source {
            MarkdownTableWindowCellSource::Present(range) => range.clone(),
            MarkdownTableWindowCellSource::ImplicitEmpty {
                insertion_anchor_utf16,
            } => *insertion_anchor_utf16..*insertion_anchor_utf16,
        })
    }

    pub(super) fn navigation(
        &self,
        document: &MarkdownDocumentBuffer,
        coordinate: NativeMarkdownTableCellCoordinate,
        navigation: NativeMarkdownTableNavigation,
    ) -> Result<MarkdownTableWindowNavigationTarget, String> {
        self.source_range(document, coordinate)?;
        let window = self
            .window
            .presentation_for(document)
            .map_err(|_| "表格窗口源已改变")?;
        let direction = match navigation {
            NativeMarkdownTableNavigation::Next => MarkdownTableWindowNavigation::Next,
            NativeMarkdownTableNavigation::Previous => MarkdownTableWindowNavigation::Previous,
            NativeMarkdownTableNavigation::NextRow => MarkdownTableWindowNavigation::NextRow,
        };
        table_window_navigation_target(
            window,
            MarkdownTableWindowCoordinate {
                row_ordinal: coordinate.row_index,
                column_index: coordinate.column_index,
            },
            direction,
        )
        .map_err(|_| "无法验证表格导航目标".into())
    }
}

#[derive(Clone)]
pub(in crate::native_surface) enum NativeMarkdownTableCellAction {
    Activate,
    ActivateOriginalClick {
        source_utf16_range: std::ops::Range<usize>,
        pointer: Option<Rc<NativeMarkdownTableCellPointer>>,
    },
    Navigate {
        original: NativeMarkdownTableCellEditorState,
        direction: NativeMarkdownTableNavigation,
    },
}

pub(in crate::native_surface) enum NativeMarkdownTableCellPoll {
    None,
    Progress,
    Blocked,
    Ready,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum NativeMarkdownTableCellPhase {
    Resolving,
    Ready,
    Committing,
    Selecting,
}

/// 一个命令一份Core scratch；派生Ready只交付纯metadata，不推进输入epoch。
#[derive(Clone)]
pub(in crate::native_surface) struct NativeMarkdownTableCellIntent {
    pub(in crate::native_surface) input_epoch: u64,
    pub(in crate::native_surface) fold_revision: u64,
    pub(in crate::native_surface) committed: Option<Rc<NativeMarkdownCommittedSelection>>,
}

impl NativeMarkdownTableCellIntent {
    pub(in crate::native_surface) fn uncommitted(input_epoch: u64, fold_revision: u64) -> Self {
        Self {
            input_epoch,
            fold_revision,
            committed: None,
        }
    }
}

pub(in crate::native_surface) struct NativeMarkdownTableCellRequest {
    pub(super) identity: WeakMarkdownDocument,
    pub(super) key: Cell<NativeMarkdownTableLayoutRequestKey>,
    pub(super) input_epoch: u64,
    pub(super) fold_revision: u64,
    pub(super) header_byte_start: usize,
    pub(super) target: NativeMarkdownTableCellCoordinate,
    pub(super) action: RefCell<NativeMarkdownTableCellAction>,
    pub(super) phase: Cell<NativeMarkdownTableCellPhase>,
    pub(super) core: RefCell<Option<MarkdownTableWindowRequest>>,
    pub(super) ready: RefCell<Option<Rc<NativeMarkdownTableCellAuthority>>>,
    pub(super) committed: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>>,
    #[cfg(feature = "native-markdown-runtime-probe")]
    observer: Option<Rc<NativeMarkdownTableCellReceipt>>,
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl Drop for NativeMarkdownTableCellRequest {
    fn drop(&mut self) {
        // clear/replace/close的旧请求只能取消自身；已有成功或失败收据不被析构覆盖。
        let committed = self.committed.get_mut().is_some();
        self.finish_observer(NativeMarkdownTableCellOutcome::Cancelled { committed });
    }
}

impl NativeMarkdownTableCellRequest {
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn finish_observer(&self, outcome: NativeMarkdownTableCellOutcome) {
        if let Some(observer) = &self.observer {
            observer.finish(outcome);
        }
    }
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn finish_activated_observer(&self) {
        let committed = self.committed.borrow().as_ref().map(Rc::clone);
        if let Some(observer) = &self.observer {
            observer.activated(self.key.get(), committed);
        }
    }

    /// 最后Native claims已经析构；同槽/原owner/源/宽度及真实active必须仍一致才能完成激活。
    pub(super) fn activation_is_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        let key = self.key.get();
        let committed = self.committed.borrow().as_ref().map(Rc::clone);
        let Some(authority) = self.authority() else {
            return false;
        };
        view.ivars().pending_table_cell_activation.owns(self)
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().table_input_intent_epoch.get() == self.input_epoch
            && committed.as_ref().map_or_else(
                || view.ivars().folds.read_is_current(self.fold_revision),
                |receipt| receipt.current(),
            )
            && view
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .is_some_and(|cache| {
                    std::rc::Weak::ptr_eq(&cache.document_identity, &self.identity)
                        && cache.request_key.document_generation == key.document_generation
                        && cache.request_key.document_length == key.document_length
                        && cache.request_key.presentation_mode == key.presentation_mode
                        && cache.request_key.text_view_width == key.text_view_width
                })
            && self.identity.upgrade().is_some_and(|document| {
                let document = document.borrow();
                document.generation() == key.document_generation
                    && document.utf16_len() == key.document_length
                    && view
                        .ivars()
                        .active_table_cell
                        .borrow()
                        .as_ref()
                        .is_some_and(|active| {
                            active.coordinate == self.target
                                && active
                                    .authority
                                    .as_ref()
                                    .is_some_and(|actual| Rc::ptr_eq(actual, &authority))
                                && authority.source_range(&document, self.target).ok().as_ref()
                                    == Some(&active.source_utf16_range)
                        })
            })
    }
    pub(super) fn new(
        document: &SharedMarkdownDocument,
        key: NativeMarkdownTableLayoutRequestKey,
        header_byte_start: usize,
        target: NativeMarkdownTableCellCoordinate,
        action: NativeMarkdownTableCellAction,
        intent: NativeMarkdownTableCellIntent,
        #[cfg(feature = "native-markdown-runtime-probe")] observer: Option<
            Rc<NativeMarkdownTableCellReceipt>,
        >,
    ) -> Result<Rc<Self>, String> {
        let NativeMarkdownTableCellIntent {
            input_epoch,
            fold_revision,
            committed,
        } = intent;
        let core = {
            let document = document.borrow();
            if document.generation() != key.document_generation
                || document.utf16_len() != key.document_length
            {
                return Err("表格请求源已经改变，草稿将保持打开".into());
            }
            let selection = document.selection();
            document
                .table_window_request(
                    MarkdownTableWindowTarget::RequiredRow {
                        header_byte_start,
                        row_ordinal: target.row_index,
                    },
                    0..0,
                    fold_revision,
                    selection.anchor_utf16.min(selection.head_utf16)
                        ..selection.anchor_utf16.max(selection.head_utf16),
                )
                .map_err(|_| "无法准备所需的真实表格行")?
        };
        Ok(Rc::new(Self {
            identity: Rc::downgrade(document),
            key: Cell::new(key),
            input_epoch,
            fold_revision,
            header_byte_start,
            target,
            action: RefCell::new(action),
            phase: Cell::new(NativeMarkdownTableCellPhase::Resolving),
            core: RefCell::new(Some(core)),
            ready: RefCell::new(None),
            committed: RefCell::new(committed),
            #[cfg(feature = "native-markdown-runtime-probe")]
            observer,
        }))
    }

    pub(super) fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        let key = self.key.get();
        if !view.ivars().pending_table_cell_activation.owns(self)
            || view.ivars().textkit_graph_teardown.get()
            || !storage.ivars().accepts_edits.get()
            || view.ivars().table_input_intent_epoch.get() != self.input_epoch
            || storage.presentation_mode() != key.presentation_mode
            || storage.ivars().table_projection_width_key.get() != key.text_view_width
            || !std::rc::Weak::ptr_eq(&self.identity, &storage.ivars().document.borrow())
        {
            return false;
        }
        let committed = self.committed.borrow().as_ref().map(Rc::clone);
        if committed.as_ref().is_some_and(|receipt| !receipt.current())
            || (committed.is_none() && !view.ivars().folds.read_is_current(self.fold_revision))
        {
            return false; // 原owner/fold或真实提交票失效后，不能重领当前位置的权限。
        }
        self.identity.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == key.document_generation
                && document.utf16_len() == key.document_length
        })
    }

    pub(super) fn authority(&self) -> Option<Rc<NativeMarkdownTableCellAuthority>> {
        self.ready.borrow().as_ref().map(Rc::clone)
    }

    /// 每次仅调用一次Core advance；本层无Native、无循环、无整表复制。
    pub(super) fn advance(
        &self,
        document: &SharedMarkdownDocument,
    ) -> Result<MarkdownTableWindowStep, String> {
        let document_ref = document.borrow();
        let mut slot = self.core.borrow_mut();
        let core = slot.as_mut().ok_or("表格行请求已经结束")?;
        let step = core
            .advance(&document_ref, false, 4096)
            .map_err(|_| "表格所需行超出预算或源已改变，草稿将保持打开")?;
        if matches!(step, MarkdownTableWindowStep::Ready { .. }) {
            let window = Rc::new(
                core.take_ready_for(&document_ref)
                    .map_err(|_| "表格行源已改变")?
                    .ok_or("所需表格行尚未就绪")?,
            );
            let authority = Rc::new(NativeMarkdownTableCellAuthority {
                document: self.identity.clone(),
                header_byte_start: self.header_byte_start,
                legacy_table_start: self.target.table_utf16_start,
                window,
            });
            let source_range = authority.source_range(&document_ref, self.target)?;
            if let NativeMarkdownTableCellAction::ActivateOriginalClick {
                source_utf16_range, ..
            } = &*self.action.borrow()
                && *source_utf16_range != source_range
            {
                // 仅物理H和逻辑坐标相同还不够；原点击源必须由唯一提交映射后完整存活。
                return Err("原点击单元格的源范围已经改变".into());
            }
            let previous = self.ready.replace(Some(authority));
            drop(previous);
            self.phase.set(NativeMarkdownTableCellPhase::Ready);
            let previous = slot.take();
            drop(slot);
            drop(previous); // Ready移交后立即释放有限行文本、Parser和源producer scratch。
        }
        Ok(step)
    }
}

impl PieceBackedMarkdownTextView {
    /// 首Native前固定本动作epoch/fold；旧getter返回后不能给它补签后继权限。
    pub(in crate::native_surface) fn start_required_table_cell(
        &self,
        target: NativeMarkdownTableCellCoordinate,
        action: NativeMarkdownTableCellAction,
        intent: NativeMarkdownTableCellIntent,
        known_header: Option<usize>,
        origin_current: &impl Fn() -> bool,
    ) -> Result<(), String> {
        self.start_required_table_cell_observed(
            target,
            action,
            intent,
            known_header,
            origin_current,
            #[cfg(feature = "native-markdown-runtime-probe")]
            None,
        )
    }

    pub(super) fn start_required_table_cell_observed(
        &self,
        target: NativeMarkdownTableCellCoordinate,
        action: NativeMarkdownTableCellAction,
        intent: NativeMarkdownTableCellIntent,
        known_header: Option<usize>,
        origin_current: &impl Fn() -> bool,
        #[cfg(feature = "native-markdown-runtime-probe")] observer: Option<
            Rc<NativeMarkdownTableCellReceipt>,
        >,
    ) -> Result<(), String> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let NativeMarkdownTableCellIntent {
            input_epoch: epoch,
            fold_revision,
            ref committed,
        } = intent;
        let initial = || {
            origin_current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == epoch
                && committed.as_ref().map_or_else(
                    || self.ivars().folds.read_is_current(fold_revision),
                    |receipt| receipt.current(),
                )
        };
        let storage =
            native_markdown_table_intent_step(&initial, || self.ivars().text_storage.load())
                .map_err(|_| "表格激活意图已经改变")?
                .ok_or("表格所属文档已关闭")?;
        let key =
            native_markdown_table_intent_step(&initial, || self.current_table_layout_request_key())
                .map_err(|_| "表格激活意图已经改变")?
                .ok_or("无法取得当前表格布局身份")?;
        if key.presentation_mode != MarkdownPresentationMode::InlinePreview {
            return Err("源码模式不能激活表格编辑字段".into());
        }
        let document = storage.document().ok_or("表格所属文档已关闭")?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        if observer.as_ref().is_some_and(|receipt| {
            !receipt.matches(self, &Rc::downgrade(&document), key, target, &intent)
        }) {
            return Err("表格激活收据不属于原请求".into());
        }
        let header = match known_header {
            Some(header) => header,
            None => {
                let document = document.borrow();
                let byte = document
                    .byte_offset_for_utf16(target.table_utf16_start)
                    .map_err(|_| "原表格坐标不是有效源位置")?;
                let line = document
                    .line_for_byte(byte)
                    .map_err(|_| "原表格物理行已改变")?;
                document
                    .byte_offset_for_line(line)
                    .map_err(|_| "原表格物理行已改变")?
            }
        };
        let request = NativeMarkdownTableCellRequest::new(
            &document,
            key,
            header,
            target,
            action,
            intent.clone(),
            #[cfg(feature = "native-markdown-runtime-probe")]
            observer.as_ref().map(Rc::clone),
        );
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                if let Some(observer) = observer {
                    observer.finish(NativeMarkdownTableCellOutcome::Failed {
                        committed: intent.committed.is_some(),
                    });
                }
                return Err(error); // Core 请求未建立，不会再有 Drop 终点；这里只完成原观察，不重发请求。
            }
        };
        let admitted = initial()
            && self.table_input_intent_is_current(&storage, &request.identity, key, epoch);
        drop(storage); // 最后Native claim析构在发布前结束；不能跨析构把旧动作放回槽。
        drop(document);
        if !admitted || !initial() {
            return Err("表格激活意图已经改变".into());
        }
        self.ivars().pending_table_cell_activation.publish(request);
        self.schedule_visible_table_layout_update();
        Ok(())
    }

    /// executor只推进真实请求一次；Pending保留Find/Mode槽，不经过旧prepared=false终局。
    pub(in crate::native_surface) fn advance_pending_table_cell_request(
        &self,
    ) -> NativeMarkdownTableCellPoll {
        let Some(request) = self.ivars().pending_table_cell_activation.request() else {
            return NativeMarkdownTableCellPoll::None;
        };
        if request.phase.get() == NativeMarkdownTableCellPhase::Committing {
            return NativeMarkdownTableCellPoll::Blocked; // 同步Native重入不能二次提交或消费半成品。
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            self.ivars()
                .pending_table_cell_activation
                .clear_if_same(&request);
            return NativeMarkdownTableCellPoll::None;
        };
        if !request.current(self, &storage) {
            self.ivars()
                .pending_table_cell_activation
                .clear_if_same(&request);
            return NativeMarkdownTableCellPoll::None;
        }
        if request.phase.get() == NativeMarkdownTableCellPhase::Selecting {
            return NativeMarkdownTableCellPoll::None; // 真实 Shift 手势只等 drag/up，不重读源或再次激活字段。
        }
        let outcome = if request.phase.get() == NativeMarkdownTableCellPhase::Ready {
            Ok(None)
        } else {
            request
                .identity
                .upgrade()
                .ok_or("表格所属文档已关闭".to_owned())
                .and_then(|document| request.advance(&document).map(Some))
        };
        match outcome {
            Ok(Some(MarkdownTableWindowStep::Progress { .. })) => {
                NativeMarkdownTableCellPoll::Progress
            }
            Ok(Some(MarkdownTableWindowStep::NotTable { .. })) => {
                self.fail_required_table_cell(
                    &request,
                    storage,
                    "原位置已不是可验证的表格，草稿将保持打开",
                );
                NativeMarkdownTableCellPoll::None
            }
            Ok(Some(MarkdownTableWindowStep::TableEndReached { .. })) => {
                let action = request.action.borrow().clone();
                let terminal = match action {
                    NativeMarkdownTableCellAction::Navigate {
                        original,
                        direction: NativeMarkdownTableNavigation::Next,
                    } => {
                        let valid = request.identity.upgrade().is_some_and(|document| {
                            let document = document.borrow();
                            request
                                .core
                                .borrow()
                                .as_ref()
                                .and_then(|core| {
                                    core.required_row_end_for(&document).ok().flatten()
                                })
                                .is_some_and(|end| {
                                    end.header_byte_start() == request.header_byte_start
                                        && end.last_body_row_ordinal().unwrap_or(0)
                                            == original.coordinate.row_index
                                })
                        });
                        valid.then_some(original)
                    }
                    _ => None, // Return/Previous和普通激活的不存在行不授予追加权限。
                };
                if request.current(self, &storage) {
                    self.ivars()
                        .pending_table_cell_activation
                        .clear_if_same(&request);
                    #[cfg(feature = "native-table-row-append")]
                    if let Some(original) = terminal {
                        let authority = original.authority.as_ref().map(Rc::clone);
                        if let Some(authority) = authority
                            && let Err(error) = self.request_terminal_table_append(
                                original,
                                &authority,
                                request.input_epoch,
                                request.fold_revision,
                            )
                        {
                            present_native_markdown_error(&error);
                        }
                    }
                    #[cfg(not(feature = "native-table-row-append"))]
                    let _ = terminal;
                }
                NativeMarkdownTableCellPoll::Progress
            }
            Ok(_) => {
                if matches!(
                    *request.action.borrow(),
                    NativeMarkdownTableCellAction::Navigate { .. }
                ) {
                    request.phase.set(NativeMarkdownTableCellPhase::Committing);
                    match self.commit_table_cell_for_request(&request, &storage) {
                        Ok(()) => NativeMarkdownTableCellPoll::Progress,
                        Err(error) => {
                            self.fail_required_table_cell(&request, storage, &error);
                            NativeMarkdownTableCellPoll::None
                        }
                    }
                } else {
                    NativeMarkdownTableCellPoll::Ready
                }
            }
            Err(error) => {
                self.fail_required_table_cell(&request, storage, &error);
                NativeMarkdownTableCellPoll::None
            }
        }
    }

    pub(super) fn fail_required_table_cell(
        &self,
        request: &NativeMarkdownTableCellRequest,
        storage: Retained<PieceBackedMarkdownTextStorage>,
        error: &str,
    ) {
        let report = request.current(self, &storage);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let committed = request.committed.borrow().is_some();
        self.ivars()
            .pending_table_cell_activation
            .clear_if_same(request);
        drop(storage); // 失败路径也先结束最后Native claim，不让回调借到仍在析构中的字段图。
        if report {
            present_native_markdown_error(error);
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        request.finish_observer(if report {
            NativeMarkdownTableCellOutcome::Failed { committed }
        } else {
            NativeMarkdownTableCellOutcome::Cancelled { committed }
        });
    }

    /// scope只读Ready的目标源行；未完成请求不得扩大几何准备范围。
    pub(in crate::native_surface) fn pending_table_cell_source_row(
        &self,
    ) -> Option<std::ops::Range<usize>> {
        let request = self.ivars().pending_table_cell_activation.request()?;
        let storage = self.ivars().text_storage.load()?;
        if !request.current(self, &storage) {
            return None;
        }
        let authority = request.authority()?;
        let document = request.identity.upgrade()?;
        let document = document.borrow();
        authority
            .window
            .presentation_for(&document)
            .ok()?
            .row(request.target.row_index)
            .map(|row| row.utf16_range.clone())
    }

    /// 一次既有几何事务后没有真实格即明确失败，不重排请求或把缺矩阵当激活完成。
    pub(in crate::native_surface) fn finish_pending_table_cell_request(&self) {
        let Some(request) = self.ivars().pending_table_cell_activation.request() else {
            return;
        };
        if request.phase.get() != NativeMarkdownTableCellPhase::Ready {
            return;
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            self.ivars()
                .pending_table_cell_activation
                .clear_if_same(&request);
            return;
        };
        self.fail_required_table_cell(
            &request,
            storage,
            "所需单元格尚无同源稳定布局；原文、草稿或已提交的撤销历史均已保留",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(source: &str) -> (tempfile::TempDir, SharedMarkdownDocument) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("required-row.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, Rc::new(RefCell::new(document)))
    }

    fn request(
        document: &SharedMarkdownDocument,
        row: usize,
        column: usize,
        epoch: u64,
    ) -> Rc<NativeMarkdownTableCellRequest> {
        let key = {
            let document = document.borrow();
            NativeMarkdownTableLayoutRequestKey {
                document_generation: document.generation(),
                document_length: document.utf16_len(),
                presentation_mode: MarkdownPresentationMode::InlinePreview,
                visible_origin_x: 0,
                visible_origin_y: 0,
                visible_width: 100,
                visible_height: 100,
                text_view_width: 100,
                table_geometry_epoch: 0,
                native_viewport_start: 0,
                native_viewport_end: 0,
            }
        };
        NativeMarkdownTableCellRequest::new(
            document,
            key,
            0,
            NativeMarkdownTableCellCoordinate {
                table_utf16_start: 0,
                row_index: row,
                column_index: column,
            },
            NativeMarkdownTableCellAction::Activate,
            NativeMarkdownTableCellIntent::uncommitted(epoch, 0),
            #[cfg(feature = "native-markdown-runtime-probe")]
            None,
        )
        .unwrap()
    }

    fn finish(
        request: &NativeMarkdownTableCellRequest,
        document: &SharedMarkdownDocument,
    ) -> MarkdownTableWindowStep {
        for _ in 0..100_000 {
            let step = request.advance(document).unwrap();
            if !matches!(step, MarkdownTableWindowStep::Progress { .. }) {
                return step;
            }
        }
        panic!("有限纯请求未结束");
    }

    #[test]
    /// 中段真实ordinal与逻辑缺格来自opaque窗口，不能将本批Vec位置或零range冒充补格计划。
    fn required_row_keeps_absolute_identity_and_implicit_cell_kind() {
        let mut source = String::from("| H | C | D |\n| - | - | - |\n");
        for row in 1..=600 {
            source.push_str(if row == 450 {
                "| only |\n"
            } else {
                "| a | b | c |\n"
            });
        }
        let (_directory, document) = document(&source);
        let request = request(&document, 450, 2, 8);
        assert!(matches!(
            finish(&request, &document),
            MarkdownTableWindowStep::Ready { .. }
        ));
        let authority = request.authority().unwrap();
        let document_ref = document.borrow();
        let window = authority.window.presentation_for(&document_ref).unwrap();
        assert_eq!(window.rows.first().unwrap().row_ordinal, 450);
        assert!(matches!(
            window.cell(450, 2).unwrap().source,
            MarkdownTableWindowCellSource::ImplicitEmpty { .. }
        ));
        let range = authority
            .source_range(&document_ref, request.target)
            .unwrap();
        assert!(range.is_empty());
        assert!(
            authority
                .source_range(
                    &document_ref,
                    NativeMarkdownTableCellCoordinate {
                        row_index: 0,
                        ..request.target
                    }
                )
                .is_err()
        );
        assert_eq!(Rc::strong_count(&document), 1); // 请求与Ready都只保留弱正文身份。
    }

    #[test]
    /// 旧局部取消不清后继槽；关闭则同时释放坐标、Core scratch和Ready，而不延长文档寿命。
    fn pending_slot_rejects_old_clear_and_has_no_second_document_owner() {
        let (_directory, document) = document("| H |\n| - |\n| a |\n| b |\n");
        let first = request(&document, 1, 0, 2);
        let second = request(&document, 2, 0, 3);
        let pending = NativeMarkdownPendingTableCell::default();
        pending.publish(Rc::clone(&first));
        pending.publish(Rc::clone(&second));
        pending.clear_if_same(&first);
        assert_eq!(pending.get(), Some(second.target));
        assert!(pending.owns(&second));
        assert!(!pending.is_empty());
        pending.set(None);
        assert!(pending.is_empty());
        assert_eq!(Rc::strong_count(&document), 1);
    }

    #[test]
    /// 同长同代的另一root不能读原Ready；原文编辑再Undo也不能恢复旧窗口权限。
    fn ready_authority_rejects_other_note_and_undo_aba() {
        let source = "| H |\n| - |\n| a |\n";
        let (_directory, document) = document(source);
        let request = request(&document, 1, 0, 2);
        assert!(matches!(
            finish(&request, &document),
            MarkdownTableWindowStep::Ready { .. }
        ));
        let authority = request.authority().unwrap();
        let (_other_directory, other) = self::document(source);
        assert!(
            authority
                .source_range(&other.borrow(), request.target)
                .is_err()
        );
        let range = authority
            .source_range(&document.borrow(), request.target)
            .unwrap();
        let after = MarkdownTextSelection::caret(range.start + 1);
        document
            .borrow_mut()
            .replace_utf16_range(range, "x", after)
            .unwrap();
        document.borrow_mut().undo().unwrap();
        assert!(
            authority
                .source_range(&document.borrow(), request.target)
                .is_err()
        );
    }

    #[test]
    /// 同一逻辑坐标仍须匹配点击原源；错误范围不能仅凭新窗口存在就获得Ready。
    fn original_click_requires_the_exact_surviving_cell_source() {
        let (_directory, document) = document("| H |\n| - |\n| a |\n");
        let original = request(&document, 1, 0, 2);
        finish(&original, &document);
        let source = original
            .authority()
            .unwrap()
            .source_range(&document.borrow(), original.target)
            .unwrap();
        let accepted = request(&document, 1, 0, 2);
        *accepted.action.borrow_mut() = NativeMarkdownTableCellAction::ActivateOriginalClick {
            source_utf16_range: source.clone(),
            pointer: None, // 纯 Core 源权限测试不制造 Native 事件，也不执行字段激活。
        };
        assert!(matches!(
            finish(&accepted, &document),
            MarkdownTableWindowStep::Ready { .. }
        ));
        let rejected = request(&document, 1, 0, 2);
        *rejected.action.borrow_mut() = NativeMarkdownTableCellAction::ActivateOriginalClick {
            source_utf16_range: source.start..source.end + 1,
            pointer: None,
        };
        for _ in 0..100_000 {
            match rejected.advance(&document) {
                Ok(MarkdownTableWindowStep::Progress { .. }) => {}
                Err(_) => {
                    assert!(rejected.authority().is_none());
                    return;
                }
                Ok(_) => panic!("错误点击范围不能发布Ready"),
            }
        }
        panic!("有限纯请求未结束");
    }
}
