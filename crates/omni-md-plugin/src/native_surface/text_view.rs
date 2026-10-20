//! Adapts native responder/input and visible table interaction to the canonical storage contract.
//!
//! The editor owner retains this view. Its storage/layout backlinks and queued tasks are weak;
//! input depth, marked ranges and semantic cell coordinates are finite foreground metadata.

use super::*;
use objc2_app_kit::{NSTextContentManagerDelegate, NSTextContentStorageDelegate};

/// 有序查询不携带方向；多范围只说明其确实存在，不能猜测其中哪个是主选择。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeMarkdownOrderedSelection {
    Single(NSRange),
    Multiple,
}

/// 仅服务原 pending 的安装尾声；每个 Native 原语前后联合核同 owner/slot 与原调用方的纯权限。
fn pending_selection_native_step<T>(
    owner: &Rc<NativeMarkdownPendingSelectionOwner>,
    slot: &NativeMarkdownPendingSelectionSlot,
    is_current: &impl Fn() -> bool,
    work: impl FnOnce() -> T,
) -> Result<T, NativeMarkdownFoldError> {
    let current = || owner.owns_slot(slot) && owner.current() && is_current();
    if !current() {
        owner.reject();
        return Err(NativeMarkdownFoldError::Stale);
    }
    let result = work();
    if !current() {
        owner.reject();
        return Err(NativeMarkdownFoldError::Stale);
    }
    Ok(result)
}

/// Stores finite input/layout metadata and one lazily owned cell field, with weak graph backlinks.
pub(super) struct PieceBackedTextViewIvars {
    // 显式诊断才创建唯一光栅 owner；回链全部为 Weak，空槽不分配像素或布局对象。
    #[cfg(feature = "native-raster-surface-probe")]
    pub(super) raster_surface:
        RefCell<Option<Rc<super::raster_surface::NativeMarkdownRasterSurface>>>,
    #[cfg(feature = "native-raster-surface-probe")]
    raster_update_layer: bool,
    #[cfg(feature = "native-raster-surface-probe")]
    raster_layer_update_calls: Cell<u64>,
    #[cfg(feature = "native-raster-surface-probe")]
    pub(super) raster_body_layer:
        RefCell<Option<Rc<super::raster_surface::NativeMarkdownRasterBodyLayerLease>>>,
    pub(super) source_decorations: RefCell<Vec<NativeMarkdownDecorationGeometry>>,
    pub(super) source_decoration_generation: Cell<u64>,
    pub(super) source_decorations_dirty: Cell<bool>,
    // 只缓存有限Copy几何与弱文档身份，绝不保持系统fragment或正文。
    pub(super) preview_blocks: RefCell<Option<NativeMarkdownPreviewBlocks>>,
    pub(super) preview_block_fold_epoch: Cell<u64>,
    // 旧prepare失败无权清除重入后新发布的cache。
    pub(super) preview_block_intent_epoch: Cell<u64>,
    // MissingLayout仅等待一次真实super-draw完成；失败不能自行制造重试循环。
    pub(super) preview_block_attempt: RefCell<Option<NativeMarkdownPreviewBlockAttempt>>,
    pub(super) gutter: OnceCell<Weak<NativeMarkdownGutter>>,
    pub(super) folds: Rc<NativeMarkdownFoldState>,
    pub(super) text_storage: Weak<PieceBackedMarkdownTextStorage>,
    // 与原live字符串同一次构造决定；关闭实验时不为annotated getter读取Native Weak或推进fold。
    #[cfg(all(
        feature = "native-block-fragment-diagnostic",
        feature = "native-markdown-runtime-probe"
    ))]
    pub(super) frozen_values_enabled: bool,
    // The surface remains the unique TextKit graph owner. This weak layout-manager link
    // prevents a competing view-owned root from surviving session teardown.
    pub(super) text_layout_manager: OnceCell<Weak<NSTextLayoutManager>>,
    pub(super) text_container_recovery_depth: Cell<usize>,
    // Replacing a content manager may synchronously invoke setTextContainer:. During
    // teardown, refuse to restore relationships that AppKit has just disconnected.
    pub(super) textkit_graph_teardown: Cell<bool>,
    // TextKit 2's inherited NSTextView implementation can retain a stale
    // marked range when its NSString is backed by a virtual piece tree. Keep
    // only the current composition span (never its text) at the input-client
    // boundary so the next IME update replaces the provisional span in place.
    pub(super) marked_text_range: Rc<Cell<Option<NSRange>>>,
    // 存储仍是事务唯一强槽；这里只给同步 superclass 子调用提供首 Native 前的原 Weak 票。
    pub(super) input_callback: RefCell<Option<std::rc::Weak<NativeMarkdownInputTransaction>>>,
    // 纯 Rust 槽的 Weak 可在首 Native 前取得原 owner；关闭时清除，不保活 storage 或正文。
    pub(super) pending_selection_slot:
        RefCell<Option<std::rc::Weak<NativeMarkdownPendingSelectionSlot>>>,
    pub(super) unmarking_text: Cell<bool>,
    pub(super) textkit_input_depth: Cell<usize>,
    pub(super) payload_use_depth: Cell<usize>,
    // 会话唯一发布槽；同步绘制/测量只临时共享不可变 Rc，不复制整组单元格和属性。
    pub(super) visible_table_layout_cache:
        RefCell<Option<Rc<NativeMarkdownVisibleTableLayoutCache>>>,
    pub(super) table_layout_update_scheduled: Cell<bool>,
    pub(super) table_layout_rebuilding: Cell<bool>,
    // 仅当前同步动作内的Weak/标量锚点；返回前必须消费或撤销。
    pub(super) table_mutation_anchor: RefCell<Option<NativeMarkdownTableMutationAnchor>>,
    /// 每view至多一个弱正文/弱栏的有限目标；成功须等实际可见性认证。
    pub(super) find_reveal: RefCell<Option<Rc<NativeMarkdownFindRevealRequest>>>,
    /// 模式目标只有Weak正文和有限标量，不能因延后定位保活Native或正文root。
    pub(super) mode_viewport:
        RefCell<Option<Rc<super::table_layout::NativeMarkdownModeViewportRequest>>>,
    pub(super) table_input_intent_epoch: Cell<u64>,
    // 一个末格 Tab 的 Core 请求与纯提交票；真实输入或 accepted release 同步撤销唯一槽。
    #[cfg(feature = "native-table-row-append")]
    pub(super) table_append: RefCell<Option<Rc<NativeMarkdownPendingTableAppend>>>,
    pub(super) table_field_expectation: RefCell<Option<Rc<NativeMarkdownTableFieldExpectation>>>,
    pub(super) table_field_notification_checking: Cell<bool>,
    // 局部Rc先结束slot借用再retain；关闭仍take/drop唯一会话owner，不增加Native身份。
    pub(super) cell_editor: RefCell<Option<Rc<Retained<NSTextField>>>>,
    pub(super) owned_ledger: std::rc::Weak<NativeMarkdownOwnedLedger>,
    pub(super) controller: OnceCell<Weak<NativeMarkdownController>>,
    pub(super) active_table_cell: RefCell<Option<NativeMarkdownTableCellEditorState>>,
    pub(super) pending_table_cell_activation: NativeMarkdownPendingTableCell,
}

define_class!(
    // SAFETY: NSTextView retains responsibility for TextKit input, layout and drawing.
    // Overrides route history into the bounded Rust model; weak links avoid a
    // text-view/content-storage/text-storage ownership cycle.
    #[unsafe(super = NSTextView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PieceBackedTextViewIvars]
    pub(super) struct PieceBackedMarkdownTextView;

    // SAFETY: this NSTextView subclass inherits initialized NSObject identity on the main thread.
    unsafe impl NSObjectProtocol for PieceBackedMarkdownTextView {}

    // SAFETY: this NSTextView subclass receives text callbacks only while its owning surface retains the live main-thread view.
    unsafe impl NSTextDelegate for PieceBackedMarkdownTextView {}

    // SAFETY: 主线程回调只消费实际发送通知的本 view，全部 getter 保持同一原选择票据。
    unsafe impl NSTextViewDelegate for PieceBackedMarkdownTextView {
        #[unsafe(method(textViewDidChangeSelection:))]
        /// 只接受同一实际 view 的选择通知；Core 纯发布生成收据后才执行后续 Native 更新。
        fn text_view_did_change_selection(&self, notification: &NSNotification) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }
            if self.consume_pending_native_selection(notification) {
                return; // 原 owner 即使已 revoked 也阻止这个旧通知重新领取后继 capture。
            }
            let Ok(capture) = self.ivars().folds.capture_selection() else { return; };
            let Ok(Some(notification_text_view)) = selection_native_step(&capture, || {
                notification.object().and_then(|object| object.downcast::<NSTextView>().ok())
            })
            else {
                // 缺失实际发送者或 getter 中发生重入时，旧通知没有更新正文选择的权限。
                return;
            };
            if !std::ptr::eq::<NSTextView>(&*notification_text_view, self.as_super()) {
                // 其他字段或文本视图的通知不能更新本篇笔记的选择。
                return;
            }
            let Ok(Some(text_storage)) = selection_native_step(&capture, || self.ivars().text_storage.load()) else {
                // 存储已经断开或 getter 中出现新输入时，迟到通知没有业务后果。
                return;
            };
            // 本次安装只声明 before/after 两个原范围；同步通知不能再从失真的桥接方向覆盖 Core。
            let observed = self.native_ordered_selection_state_for_capture(&capture);
            if let Ok(NativeMarkdownOrderedSelection::Single(range)) = observed
                && let Some(NativeMarkdownSelectionInstallObservation::OwnDeclared) =
                    self.ivars().folds.observe_native_selection(range)
            {
                return;
            }
            let selection = match selection_native_step(&capture, || self.primary_text_selection()) {
                Ok(Ok(selection)) => selection,
                Ok(Err(NativeMarkdownFoldError::UnresolvedSelection(unresolved))) => {
                    if matches!(observed, Ok(NativeMarkdownOrderedSelection::Single(_)))
                        && capture.current() && unresolved.current()
                    {
                        // 外部单范围已改变但活动端未知；撤销旧方向，不能在随后 A→B→A 时复用旧 head。
                        // Find 全部匹配的多范围由其原调用栈完成状态交接，此处不提前撤其安装票。
                        self.ivars().folds.reset_selection();
                    }
                    return;
                }
                _ => return, // 多选主端点未知或原票已撤销时保留正文，不猜第一个升序范围。
            };
            let unchanged = text_storage.document().is_some_and(|document| {
                let document = document.borrow();
                selection.current_in(&document)
                    && document.selection() == selection.selection
                    && self.ivars().folds.known_selection() == Some(selection.selection)
                    && self.ivars().folds.known_head_is_upstream() == selection.head_is_upstream()
            });
            if unchanged {
                return; // 重复原生通知没有新事实；不能撤销同一选择上的在途定位或鼠标手势。
            }
            let Some(committed) = text_storage.update_active_selection(selection) else { return; };
            if committed.native_step(|| self.finish_fold_layout_update()).is_err()
                || committed.native_step(|| self.invalidate_source_gutter_highlight()).is_err()
                || committed.native_step(|| self.invalidate_visible_body()).is_err()
            {
                return; // 纯选择已发布；后继输入撤销的只是本通知剩余的布局与绘制后果。
            }
            // 相关背景、括号和 caret 提示共用既有弱引用布局事务，不在此同步重排正文。
            self.ivars().source_decorations_dirty.set(true);
            let _ = committed.native_step(|| self.schedule_visible_table_layout_update());
        }
    }

    // SAFETY: the owning content storage holds a weak delegate; callbacks run on the same main thread.
    unsafe impl NSTextContentManagerDelegate for PieceBackedMarkdownTextView {
        #[unsafe(method(textContentManager:shouldEnumerateTextElement:options:))]
        /// Omit only paragraphs within current-generation view-local folds, retaining canonical source offsets.
        fn should_enumerate_text_element(
            &self,
            manager: &objc2_app_kit::NSTextContentManager,
            element: &objc2_app_kit::NSTextElement,
            _options: objc2_app_kit::NSTextContentManagerEnumerationOptions,
        ) -> objc2::runtime::Bool {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            let Ok(capture) = self.ivars().folds.capture_selection() else {
                return objc2::runtime::Bool::YES; // 失去原owner或代次时必须完整可读。
            };
            if self.ivars().folds.borrow().is_empty() {
                return objc2::runtime::Bool::YES; // 无fold无需查询Native坐标。
            }
            let hidden = (|| -> Result<bool, NativeMarkdownFoldError> {
                // 旧content manager的迟到delegate回调不能将新fold应用到另一套element坐标。
                let layout = selection_native_step(&capture, || {
                    self.ivars().text_layout_manager.get().and_then(Weak::load)
                })?.ok_or(NativeMarkdownFoldError::Stale)?;
                let content = selection_native_step(&capture, || layout.textContentManager())?
                    .ok_or(NativeMarkdownFoldError::Stale)?;
                if !std::ptr::eq(&*content, manager) {
                    return Err(NativeMarkdownFoldError::Stale);
                }
                let Some(range) = selection_native_step(&capture, || element.elementRange())? else {
                    return Ok(false); // 系统非文本结构没有可授权的隐藏区间。
                };
                let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(manager);
                let document_range = selection_native_step(&capture, || provider.documentRange())?;
                let origin = selection_native_step(&capture, || document_range.location())?;
                let location = selection_native_step(&capture, || range.location())?;
                let offset = selection_native_step(&capture, || {
                    manager.offsetFromLocation_toLocation(&origin, &location)
                })?;
                let offset = usize::try_from(offset)
                    .map_err(|_| NativeMarkdownFoldError::InvalidSelection)?;
                // 最后一次Native已通过原票据；有限纯查询不会在借用内触发getter或析构。
                Ok(self.ivars().folds.borrow().iter()
                    .any(|fold| fold.hidden_utf16_range.contains(&offset)))
            })();
            // 任一getter重入变更都会撤原票；不得用旧element位置裁掉当前正文。
            if !capture.current() {
                return objc2::runtime::Bool::YES; // 闭包中Native临时对象已析构，仍核同一原票。
            }
            hidden.map_or(objc2::runtime::Bool::YES, |hidden| (!hidden).into())
        }
    }

    // SAFETY: this storage-specific delegate inherits the same weak, main-thread content-manager contract.
    unsafe impl NSTextContentStorageDelegate for PieceBackedMarkdownTextView {}

    // SAFETY: this NSTextView subclass serves only its retained table-cell field and returns finite main-thread command results.
    unsafe impl NSControlTextEditingDelegate for PieceBackedMarkdownTextView {
        #[unsafe(method(controlTextDidBeginEditing:))]
        /// 表格字段真实开始编辑时记录窗口独占历史，不在关闭时查询 manager。
        fn table_control_did_begin_editing(&self, notification: &NSNotification) {
            self.observe_table_field_editing(notification);
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        /// 表格正常离开字段后结束其自有历史；校验失败仍保留活动字段的 Undo/Redo。
        fn table_control_did_end_editing(&self, notification: &NSNotification) {
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok())
                && let Some(controller) = self.ivars().controller.get().and_then(Weak::load)
            {
                // 与 Find/search 同一 client 边界，不改表格提交/取消和 canonical history。
                let borrowed = controller.ivars().borrowed.borrow().clone();
                let _ = borrowed.finished_control_editing(&field, &controller.ivars().undo, &controller.ivars().owned);
                controller.queue_history_availability_refresh(); // 原字段已真实结束，再显示按钮后继将操作的 Core 历史。
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        /// 标记和已提交编辑均经同一实际字段记录历史归属。
        fn table_control_did_change(&self, notification: &NSNotification) {
            if !self.table_field_did_change_input(notification) {
                return; // 外来或已释放字段没有取消末格动作或记录本会话历史的权限。
            }
            if let Some(active) = self.ivars().active_table_cell.borrow_mut().as_mut() {
                active.field_selection = None; // 新文字不能凭恰巧相同的有序范围继承旧非空方向；后续真实 caret 可立即重建。
            }
            self.observe_table_field_editing(notification);
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        /// Handles spreadsheet navigation for the one reusable table-cell field.
        ///
        /// # Safety
        /// AppKit must provide live main-thread control/text-view borrows and a valid Objective-C selector.
        unsafe fn control_text_view_do_command_by_selector(
            &self,
            _control: &NSControl,
            _text_view: &NSTextView,
            command_selector: Sel,
        ) -> bool {
            if self.ivars().textkit_graph_teardown.get() {
                // 已接受关闭的旧字段命令被消费，不回落为新的输入或表格导航。
                return true.into();
            }
            if !self.table_field_command_is_current(_control, _text_view) {
                return true.into(); // 迟到的旧共享 editor 不得触发另一个字段的 Tab 或 Escape。
            }
            if command_selector == sel!(insertTab:) {
                // Tab commits the current draft before moving horizontally to the next parsed cell.
                self.navigate_active_table_cell(NativeMarkdownTableNavigation::Next);
                true
            } else if command_selector == sel!(insertBacktab:) {
                // Shift-Tab follows the same commit path in the reverse semantic direction.
                self.navigate_active_table_cell(NativeMarkdownTableNavigation::Previous);
                true
            } else if command_selector == sel!(insertNewline:)
                || command_selector == sel!(insertLineBreak:)
            {
                // Enter preserves the column while moving only to an existing next row.
                self.navigate_active_table_cell(NativeMarkdownTableNavigation::NextRow);
                true
            } else if command_selector == sel!(cancelOperation:) {
                // The cell field owns this cancellation; retain the original source and document window.
                self.cancel_active_table_cell();
                true
            } else {
                // Unclaimed field-editor commands retain standard native text behavior.
                false
            }
        }
    }

    // SAFETY: this NSTextView subclass uses the same main-thread table-field delegate lifetime as its owning text view.
    unsafe impl NSTextFieldDelegate for PieceBackedMarkdownTextView {}

    impl PieceBackedMarkdownTextView {
        #[cfg(feature = "native-block-fragment-diagnostic")]
        #[unsafe(method(blockUnsupported:))]
        /// exact content的公开unsupported事件；只用于判断候选仍存在兼容回退。
        fn block_unsupported(&self, _note: &NSNotification) { super::preview_blocks::diagnostic::notification(0); }

        #[cfg(feature = "native-block-fragment-diagnostic")]
        #[unsafe(method(blockWillSwitch:))]
        /// 实际body view准备切换的公开通知，不阻止或重置框架行为。
        fn block_will_switch(&self, _note: &NSNotification) { super::preview_blocks::diagnostic::notification(1); }

        #[cfg(feature = "native-block-fragment-diagnostic")]
        #[unsafe(method(blockDidSwitch:))]
        /// 实际body view已切换的公开通知，不能把它误记成生产修复成功。
        fn block_did_switch(&self, _note: &NSNotification) { super::preview_blocks::diagnostic::notification(2); }

        #[unsafe(method(selectAll:))]
        /// 全选明确整个 canonical 范围，不能把多范围的第一项当作主选择。
        fn select_all(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if let Some(forward) = self.run_semantic_selection_expansion(NativeMarkdownSelectionExpansion::All)
                && forward.current(self)
            {
                // 标记文本仍归输入法所有；同一原动作只交回匹配的系统 selector 一次。
                // SAFETY: sender 由当前主线程 responder 回调借用，payload 租约覆盖整个 super 栈。
                unsafe { let _: () = msg_send![super(self), selectAll: sender]; }
            }
        }

        #[unsafe(method(selectWord:))]
        /// 选词由原 TextKit 图给出完整边界，保留 Unicode 和系统分词规则。
        fn select_word(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if let Some(forward) = self.run_semantic_selection_expansion(NativeMarkdownSelectionExpansion::Word)
                && forward.current(self)
            {
                // 标记文本仍归输入法所有；同一原动作只交回匹配的系统 selector 一次。
                // SAFETY: sender 由当前主线程 responder 回调借用，payload 租约覆盖整个 super 栈。
                unsafe { let _: () = msg_send![super(self), selectWord: sender]; }
            }
        }

        #[unsafe(method(selectLine:))]
        /// 选行使用实际视觉行粒度，软换行不能近似为整个段落。
        fn select_line(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if let Some(forward) = self.run_semantic_selection_expansion(NativeMarkdownSelectionExpansion::Line)
                && forward.current(self)
            {
                // 标记文本仍归输入法所有；同一原动作只交回匹配的系统 selector 一次。
                // SAFETY: sender 由当前主线程 responder 回调借用，payload 租约覆盖整个 super 栈。
                unsafe { let _: () = msg_send![super(self), selectLine: sender]; }
            }
        }

        #[unsafe(method(selectParagraph:))]
        /// 选段使用实际段落边界，不扫描或复制整段文本。
        fn select_paragraph(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if let Some(forward) = self.run_semantic_selection_expansion(NativeMarkdownSelectionExpansion::Paragraph)
                && forward.current(self)
            {
                // 标记文本仍归输入法所有；同一原动作只交回匹配的系统 selector 一次。
                // SAFETY: sender 由当前主线程 responder 回调借用，payload 租约覆盖整个 super 栈。
                unsafe { let _: () = msg_send![super(self), selectParagraph: sender]; }
            }
        }

        #[unsafe(method(moveForward:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_forward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveForward: sender]; }
            }
        }

        #[unsafe(method(moveForwardAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_forward_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveForwardAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveBackward:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_backward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveBackward: sender]; }
            }
        }

        #[unsafe(method(moveBackwardAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_backward_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveBackwardAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveRight:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_right(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Right,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveRight: sender]; }
            }
        }

        #[unsafe(method(moveRightAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_right_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Right,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveRightAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveLeft:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_left(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Left,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveLeft: sender]; }
            }
        }

        #[unsafe(method(moveLeftAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_left_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Left,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveLeftAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveUp:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_up(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Up,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveUp: sender]; }
            }
        }

        #[unsafe(method(moveUpAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_up_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Up,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveUpAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveDown:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_down(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Down,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveDown: sender]; }
            }
        }

        #[unsafe(method(moveDownAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_down_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Down,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Character,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveDownAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveWordForward:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_forward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordForward: sender]; }
            }
        }

        #[unsafe(method(moveWordForwardAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_forward_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordForwardAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveWordBackward:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_backward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordBackward: sender]; }
            }
        }

        #[unsafe(method(moveWordBackwardAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_backward_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordBackwardAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveWordRight:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_right(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Right,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordRight: sender]; }
            }
        }

        #[unsafe(method(moveWordRightAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_right_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Right,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordRightAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveWordLeft:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_left(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Left,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordLeft: sender]; }
            }
        }

        #[unsafe(method(moveWordLeftAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_word_left_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Left,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Word,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveWordLeftAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToBeginningOfLine:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_beginning_of_line(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToBeginningOfLine: sender]; }
            }
        }

        #[unsafe(method(moveToBeginningOfLineAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_beginning_of_line_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToBeginningOfLineAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToEndOfLine:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_end_of_line(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToEndOfLine: sender]; }
            }
        }

        #[unsafe(method(moveToEndOfLineAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_end_of_line_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToEndOfLineAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToLeftEndOfLine:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_left_end_of_line(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Left,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToLeftEndOfLine: sender]; }
            }
        }

        #[unsafe(method(moveToLeftEndOfLineAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_left_end_of_line_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Left,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToLeftEndOfLineAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToRightEndOfLine:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_right_end_of_line(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Right,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToRightEndOfLine: sender]; }
            }
        }

        #[unsafe(method(moveToRightEndOfLineAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_right_end_of_line_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Right,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Line,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToRightEndOfLineAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToBeginningOfParagraph:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_beginning_of_paragraph(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToBeginningOfParagraph: sender]; }
            }
        }

        #[unsafe(method(moveToBeginningOfParagraphAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_beginning_of_paragraph_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToBeginningOfParagraphAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToEndOfParagraph:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_end_of_paragraph(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToEndOfParagraph: sender]; }
            }
        }

        #[unsafe(method(moveToEndOfParagraphAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_end_of_paragraph_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToEndOfParagraphAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveParagraphBackward:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_paragraph_backward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveParagraphBackward: sender]; }
            }
        }

        #[unsafe(method(moveParagraphBackwardAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_paragraph_backward_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveParagraphBackwardAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveParagraphForward:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_paragraph_forward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveParagraphForward: sender]; }
            }
        }

        #[unsafe(method(moveParagraphForwardAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_paragraph_forward_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Paragraph,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveParagraphForwardAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToBeginningOfDocument:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_beginning_of_document(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Document,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToBeginningOfDocument: sender]; }
            }
        }

        #[unsafe(method(moveToBeginningOfDocumentAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_beginning_of_document_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Backward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Document,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToBeginningOfDocumentAndModifySelection: sender]; }
            }
        }

        #[unsafe(method(moveToEndOfDocument:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_end_of_document(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Document,
                extending: false,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToEndOfDocument: sender]; }
            }
        }

        #[unsafe(method(moveToEndOfDocumentAndModifySelection:))]
        /// 系统既有选择动作交同一 Core 方向协调；IME 标记区仍沿原 selector 处理。
        fn move_to_end_of_document_and_modify_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.run_semantic_selection_motion(NativeMarkdownSelectionMotion {
                direction: objc2_app_kit::NSTextSelectionNavigationDirection::Forward,
                destination: objc2_app_kit::NSTextSelectionNavigationDestination::Document,
                extending: true,
            }).is_some_and(|forward| forward.current(self)) {
                // SAFETY: 同一主线程活 view 仅转发原动作一次；返回后原 IME 票仍有效才进入 super。
                unsafe { let _: () = msg_send![super(self), moveToEndOfDocumentAndModifySelection: sender]; }
            }
        }


        #[unsafe(method(keyDown:))]
        /// 实际按键先撤销旧安装权限，再由系统解释；诊断只关联本次真实 keyDown 调用栈。
        fn traced_key_down(&self, event: &NSEvent) {
            // 输入解释或 trace scope 尚在栈内时，资源预检不能接受旧业务结束。
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }

            self.ivars().folds.invalidate_native_install();
            // SAFETY: the original live AppKit event is forwarded unchanged to the matching main-thread
            // superclass method exactly once. Standard input-manager interpretation remains native.
            unsafe {
                let _: () = msg_send![super(self), keyDown: event];
            }
        }

        #[unsafe(method(updateDragTypeRegistration))]
        /// Registers only the plain-source payload types this adapter can import, with no rich-text fallback.
        fn update_drag_type_registration(&self) {
            // This plain-source subclass owns file-to-Markdown imports. The inherited
            // rich-text policy would unregister them and enumerate a compatibility layout graph.
            if self.ivars().textkit_graph_teardown.get() || !self.isEditable() {
                // Closing/read-only views accept no drops and may not recreate layout relationships.
                self.unregisterDraggedTypes();
            } else {
                // Register only the native plain-text/file payloads handled by this adapter.
                // SAFETY: AppKit exports immutable process-lifetime type identifiers.
                let types = unsafe {
                    [
                        objc2_app_kit::NSPasteboardTypeFileURL,
                        NSPasteboardTypeString,
                    ]
                };
                self.registerForDraggedTypes(&NSArray::from_slice(&types));
            }
        }

        // 独立 TextKit 参考图仅接受原生文字机制输入；正式附件粘贴/拖入由分页 Input 唯一处理。
        // 此处继承 NSTextView 的标准文字入口，不再把参考视图接回正式窗口的附件导入队列。

        #[unsafe(method(setTextContainer:))]
        /// Preserves the existing weak layout-manager relation during native container callbacks.
        ///
        /// # Safety
        /// AppKit must supply a live optional NSTextContainer on this view's allocating main thread.
        unsafe fn set_text_container(&self, text_container: Option<&NSTextContainer>) {
            let graph_teardown = self.ivars().textkit_graph_teardown.get();
            native_markdown_trace! {
                eprintln!(
                    "[probe] setTextContainer before present={} container={:?} container_manager={} manager={} content={} teardown={}",
                    text_container.is_some(),
                    text_container.map(|value| value as *const NSTextContainer),
                    text_container.is_some_and(|value| value.textLayoutManager().is_some()),
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                    graph_teardown,
                );
            }
            // SAFETY: AppKit owns the optional container through this main-thread callback;
            // forward the exact public setter contract once before repairing the weak manager link.
            unsafe {
                let _: () = msg_send![super(self), setTextContainer: text_container];
            }
            if !graph_teardown
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().text_container_recovery_depth.get() == 0
                && let Some(text_container) = text_container
                && let Some(text_layout_manager) =
                    self.ivars().text_layout_manager.get().and_then(Weak::load)
            {
                // AppKit temporarily detaches the container from both view and manager while resizing.
                // A callback-local retain restores the manager link without creating a second
                // long-lived ownership root alongside the surface.
                self.ivars().text_container_recovery_depth.set(1);
                text_layout_manager.setTextContainer(Some(text_container));
                self.ivars().text_container_recovery_depth.set(0);
            }
            native_markdown_trace! {
                eprintln!(
                    "[probe] setTextContainer after manager={} content={} teardown={}",
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                    self.ivars().textkit_graph_teardown.get(),
                );
            }
        }

        #[unsafe(method(replaceTextContainer:))]
        /// Forwards an AppKit-requested container replacement without creating a competing layout owner.
        fn replace_text_container(&self, text_container: &NSTextContainer) {
            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }

            native_markdown_trace! {
                eprintln!(
                    "[probe] replaceTextContainer before manager={} content={}",
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                );
            }
            // SAFETY: forward the live borrowed container to the matching superclass method on the main thread.
            unsafe {
                let _: () = msg_send![super(self), replaceTextContainer: text_container];
            }
            native_markdown_trace! {
                eprintln!(
                    "[probe] replaceTextContainer after manager={} content={}",
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                );
            }
        }

        #[unsafe(method(viewDidMoveToWindow))]
        /// 先撤销旧窗口的光栅几何权限，再完成原窗口挂接与有限布局调度。
        fn view_did_move_to_window(&self) {
            #[cfg(feature = "native-raster-surface-probe")]
            {
                let owner = self.ivars().raster_surface.borrow().clone();
                if let Some(owner) = owner {
                    owner.invalidate_geometry(); // 纯标量撤票发生在任何 Native getter 和父类回调之前。
                }
            }
            native_markdown_trace! {
                eprintln!(
                    "[probe] view did move window before manager={} window={}",
                    self.textLayoutManager().is_some(),
                    self.window().is_some()
                );
            }
            // Complete superclass window association before scheduling bounded table layout.
            // Layout runs later on the main queue, outside this callback and the drawing stack.
            // SAFETY: this main-thread override invokes its matching no-argument superclass callback once.
            unsafe {
                let _: () = msg_send![super(self), viewDidMoveToWindow];
            }
            self.schedule_visible_table_layout_update();
            native_markdown_trace! {
                eprintln!(
                    "[probe] view did move window after manager={} window={}",
                    self.textLayoutManager().is_some(),
                    self.window().is_some()
                );
            }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(viewDidChangeBackingProperties))]
        /// 实际 backing 变化撤销原像素比例票；仍由系统完成原有屏幕适配。
        fn view_did_change_backing_properties(&self) {
            let owner = self.ivars().raster_surface.borrow().clone();
            if let Some(owner) = owner {
                owner.invalidate_geometry(); // 旧比例不能在父类重入后继续发布为当前视口。
            }
            // SAFETY: 活主线程 NSView 只向匹配的公开无参父类回调转发一次。
            unsafe { let _: () = msg_send![super(self), viewDidChangeBackingProperties]; }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(invalidateTextContainerOrigin))]
        /// 系统发出的 container 原点失效先撤原票，再保留 NSTextView 的原位置计算。
        fn invalidate_text_container_origin(&self) {
            let owner = self.ivars().raster_surface.borrow().clone();
            if let Some(owner) = owner {
                owner.invalidate_geometry(); // 不自行移动正文、补签权限或强制全篇排版。
            }
            // SAFETY: 主线程已初始化的 NSTextView 调用同名公开父类入口一次。
            unsafe { let _: () = msg_send![super(self), invalidateTextContainerOrigin]; }
        }

        #[cfg(feature = "native-markdown-runtime-probe")]
        #[unsafe(method(setFrame:))]
        /// 只观察系统实际送入的完整frame参数；不读回几何，匹配super始终只转发一次。
        fn set_frame(&self, new_frame: NSRect) {
            let reentry = (!self.ivars().textkit_graph_teardown.get()).then(|| {
                let callback = super::viewport_reentry_diagnostic::geometry_rect_callback(
                    super::viewport_reentry_diagnostic::GeometryKind::FrameRect,
                    new_frame,
                );
                callback.record(self.ivars().table_layout_update_scheduled.get());
                callback
            });
            let phase = super::viewport_reentry_diagnostic::enter(super::viewport_reentry_diagnostic::Phase::FrameRectSuper);
            // SAFETY: 原NSRect按公开setFrame: ABI传给同一活主线程view的super一次，不修改参数。
            unsafe { let _: () = msg_send![super(self), setFrame: new_frame]; }
            drop(phase);
            drop(reentry); // 原Native栈返回后恢复中断phase，不安排任何显示或布局工作。
        }

        #[cfg(feature = "native-markdown-runtime-probe")]
        #[unsafe(method(setBounds:))]
        /// 只观察系统实际送入的完整bounds参数，不把未命中的C内部更新假定为无变化。
        fn set_bounds(&self, new_bounds: NSRect) {
            let reentry = (!self.ivars().textkit_graph_teardown.get()).then(|| {
                let callback = super::viewport_reentry_diagnostic::geometry_rect_callback(
                    super::viewport_reentry_diagnostic::GeometryKind::BoundsRect,
                    new_bounds,
                );
                callback.record(self.ivars().table_layout_update_scheduled.get());
                callback
            });
            let phase = super::viewport_reentry_diagnostic::enter(super::viewport_reentry_diagnostic::Phase::BoundsRectSuper);
            // SAFETY: 只转发实际NSRect到公开setBounds:一次；不调用其他setter或读取后继Native状态。
            unsafe { let _: () = msg_send![super(self), setBounds: new_bounds]; }
            drop(phase);
            drop(reentry);
        }

        #[unsafe(method(setFrameSize:))]
        /// 保留原生尺寸计算，逻辑文档增高只标脏当次真实可见交集。
        fn set_frame_size(&self, new_size: NSSize) {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let reentry = (!self.ivars().textkit_graph_teardown.get()).then(|| {
                let callback = super::viewport_reentry_diagnostic::geometry_callback(
                    super::viewport_reentry_diagnostic::GeometryKind::FrameSize,
                    Some((new_size.width, new_size.height)),
                );
                callback.record(self.ivars().table_layout_update_scheduled.get());
                callback
            });
            // 改宽先撤旧权限；TextKit自己的内容高度更新不能被误当作用户新意图。
            self.cancel_mode_viewport_for_width(new_size.width);
            self.cancel_find_reveal_for_width(new_size.width);
            let storage = {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(super::viewport_reentry_diagnostic::Phase::FrameStorage);
                self.ivars().text_storage.load()
            };
            if let Some(storage) = storage {
                // 先撤销旧列宽的属性/geometry权限，再让 superclass 可能重入布局；不发原生通知。
                storage.invalidate_table_projection_width(native_markdown_layout_metric(new_size.width));
            }
            native_markdown_trace! {
                eprintln!(
                    "[probe] set frame size before {:?} manager={}",
                    new_size,
                    {
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        let _phase = super::viewport_reentry_diagnostic::enter(super::viewport_reentry_diagnostic::Phase::FrameTraceBefore);
                        self.textLayoutManager().is_some()
                    }
                );
            }
            // SAFETY: 当前主线程活 view 将原 NSSize 转发匹配的 superclass 方法一次。
            unsafe {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(super::viewport_reentry_diagnostic::Phase::FrameSuper);
                let _: () = msg_send![super(self), setFrameSize: new_size];
            }
            // OnSetNeedsDisplay 不因整篇逻辑高度变化自动重绘；宽度和可见区域变化仍须交付真实文字。
            // 原helper同时检查关闭和有限可见几何，不用整篇bounds请求新的正文backing。
            self.invalidate_visible_body();
            // 字节未变的缩放同样撤销旧视口几何，仍由原有续体合并表格布局工作。
            {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(super::viewport_reentry_diagnostic::Phase::FrameSchedule);
                self.schedule_visible_table_layout_update();
            }
            native_markdown_trace! {
                eprintln!(
                    "[probe] set frame size after {:?} manager={}",
                    new_size,
                    {
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        let _phase = super::viewport_reentry_diagnostic::enter(super::viewport_reentry_diagnostic::Phase::FrameTraceAfter);
                        self.textLayoutManager().is_some()
                    }
                );
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            drop(reentry); // 所有原Native调用与临时claim结束后再恢复外层phase，不影响原几何结果。
        }

        #[unsafe(method(viewDidEndLiveResize))]
        /// Queues one coalesced geometry refresh after native live resizing finishes.
        fn view_did_end_live_resize(&self) {
            native_markdown_trace! {
                eprintln!("[probe] view end resize manager={}", self.textLayoutManager().is_some());
            }
            // SAFETY: the live main-thread view forwards its matching no-argument superclass callback once.
            unsafe {
                let _: () = msg_send![super(self), viewDidEndLiveResize];
            }
            self.schedule_visible_table_layout_update();
        }

        #[unsafe(method(drawViewBackgroundInRect:))]
        /// 原生背景完成后、正文glyph绘制前消费已验证的有限块与活动行几何。
        fn draw_view_background_in_rect(&self, dirty: NSRect) {
            if self.ivars().textkit_graph_teardown.get() {
                // 关闭已撤销正文绘制权；系统迟到的背景回调不能重新查询已拆除的 TextKit 图。
                return;
            }
            // 画图栈可能暂持可见正文/属性，外部关闭必须等这个同步租约结束。
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("background_super_begin");
            // SAFETY: 主线程活NSTextView仅将同一有限dirty转交匹配的公开背景super入口一次。
            unsafe {
                let _: () = msg_send![super(self), drawViewBackgroundInRect: dirty];
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("background_super_end");
            // super背景已完成而native glyph尚未绘制；这里只消费已验证的有限矩形。
            self.draw_preview_blocks(dirty);
            self.draw_source_active_line(dirty);
            #[cfg(feature = "native-block-fragment-diagnostic")]
            if let Some(storage) = self.ivars().text_storage.load() {
                // Source也记录同一paint阶段，只有此前明确mode切换才输出。
                storage.ivars().mode_attribute_diagnostic.report(2);
            }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(viewWillDraw))]
        /// 消费真实显示周期准备有限图像几何，补足窗口挂接后尚未再次 layout 的首帧。
        fn view_will_draw(&self) {
            if self.ivars().textkit_graph_teardown.get() {
                // 已接受关闭的空壳无需准备下一帧，避免父类再次排版旧正文。
                return;
            }
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            let owner = self.ivars().raster_surface.borrow().clone();
            let origin = owner.as_ref().map(|owner| owner.begin(self));
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("will_draw_super_begin");
            // 显示准备会产生由框架暂时持有的 viewport view；同步入口自己的临时 claim
            // 必须在该入口返回前归还，不能留在应用外围池而继续保活旧 fragment。
            autoreleasepool(|_| {
                // SAFETY: 活主线程 NSView 将实际无参显示通知向对应父类转发一次。
                unsafe { let _: () = msg_send![super(self), viewWillDraw]; }
            });
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("will_draw_super_end");
            if let Some(owner) = owner {
                match origin.expect("光栅 owner 与原显示入口同时存在") {
                    Ok(Some(permit)) => {
                        // 不调 ensure 或强制 layout；只消费本次显示前已存在的真实视口。
                        if let Err(error) = owner.settle_layout(self, &permit) {
                            eprintln!("native_raster_surface event=will_draw_geometry_failed error={error:?} formal=false");
                        }
                    }
                    Ok(None) => {} // 构造未就绪或真实回调撤票，交回系统下一次原显示周期。
                    Err(error) => {
                        eprintln!("native_raster_surface event=will_draw_origin_failed error={error:?} formal=false");
                    }
                }
            }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(layout))]
        /// 原布局完成后才更新预建视口图像的几何；绘制回调不能反向移动正文或光栅视图。
        fn layout(&self) {
            if self.ivars().textkit_graph_teardown.get() {
                // 拆图后不再向父类发起正文布局；窗口自己的生命周期继续由原控制器处理。
                return;
            }
            // 与既有绘制相同，同步布局未结束前关闭请求只能等待，不能拆掉正在消费的 graph。
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            let owner = self.ivars().raster_surface.borrow().clone();
            // 父类也可同步触发输入；必须在它之前固定原票，之后不能领取改变后的状态。
            let origin = owner.as_ref().map(|owner| owner.begin(self));
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("layout_super_begin");
            // AppKit 直接交付的布局回调不经过项目队列池；其临时视口对象在同一原操作内归还。
            autoreleasepool(|_| {
                // SAFETY: 活主线程 NSView 将无参数的 layout 同步转发给匹配父类一次。
                unsafe { let _: () = msg_send![super(self), layout]; }
            });
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("layout_super_end");
            if let Some(owner) = owner {
                let permit = match origin.expect("光栅 owner 与原入口观察同时存在") {
                    Ok(Some(permit)) => permit,
                    Ok(None) => return, // 构造未就绪或原动作取消，没有更新图像几何的权限。
                    Err(error) => {
                        eprintln!("native_raster_surface event=layout_origin_failed error={error:?} formal=false");
                        return;
                    }
                };
                // Rc 槽借用已结束；只消费父类调用前的原票，Native 变化不会被新的捕获掩盖。
                if let Err(error) = owner.settle_layout(self, &permit) {
                    eprintln!("native_raster_surface event=layout_failed error={error:?} formal=false");
                }
            }
        }

        #[cfg(all(feature = "native-raster-surface-probe", feature = "native-markdown-runtime-probe"))]
        #[unsafe(method(wantsUpdateLayer))]
        /// 只有构造时显式选择的光栅对照绕开父视图drawRect backing；默认仍问原super。
        fn wants_update_layer(&self) -> bool {
            if self.ivars().raster_update_layer {
                true // 纯标量，不在属性getter内读取正文、布局或图层几何。
            } else {
                // SAFETY: 主线程活NSTextView询问同一公开BOOL属性，保持默认原生绘制策略。
                unsafe { msg_send![super(self), wantsUpdateLayer] }
            }
        }

        #[cfg(all(feature = "native-raster-surface-probe", feature = "native-markdown-runtime-probe"))]
        #[unsafe(method(updateLayer))]
        /// 真正layer回调只更新既有共享视口图像，不更改几何或视图树，也不假设有窗口CGContext。
        fn update_layer(&self) {
            if self.ivars().textkit_graph_teardown.get() {
                return; // accepted关闭已撤业务绘制权，迟到layer更新不能恢复原图。
            }
            if !self.ivars().raster_update_layer {
                // SAFETY: 默认路径只转发匹配的公开无参入口一次；不接管原Native更新。
                unsafe { let _: () = msg_send![super(self), updateLayer]; }
                return;
            }
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            let owner = self.ivars().raster_surface.borrow().clone();
            let Some(owner) = owner else {
                return; // 尚未挂载或已经移除唯一owner，不能把空回调签成完成。
            };
            let permit = match owner.begin(self) {
                Ok(Some(permit)) => permit,
                Ok(None) => return, // 原入口未就绪/已撤销，不在同一栈补签后继票。
                Err(error) => {
                    eprintln!("native_raster_surface event=layer_origin_failed error={error:?} formal=false");
                    return;
                }
            };
            let Some(sequence) = self.ivars().raster_layer_update_calls.get().checked_add(1) else {
                owner.fail_layer_update_sequence();
                return; // 耗尽必须失败，不能回绕把后继回调记为首次更新。
            };
            self.ivars().raster_layer_update_calls.set(sequence);
            if sequence <= 8 {
                eprintln!("native_raster_surface event=layer_update_begin sequence={sequence} formal=false");
            }
            match owner.paint_layer(self, &permit) {
                Ok(true) => {} // 实际同源CPU画布已发布，才开放原native-draw完成通知。
                Ok(false) => return, // 没有实际发布时不能推进Find/背景准备或模式终点。
                Err(error) => {
                    eprintln!("native_raster_surface event=layer_paint_failed error={error:?} formal=false");
                    return;
                }
            }
            self.preview_blocks_completed_native_draw();
            self.schedule_visible_table_layout_update(); // 仍是既有合并后继，不在layer栈强制排版。
            if sequence <= 8 {
                eprintln!("native_raster_surface event=layer_update_completed sequence={sequence} formal=false");
            }
        }

        #[unsafe(method(drawRect:))]
        /// 原生正文完成后绘制既有有限表格与Source装饰，并只排原coalesced后继。
        fn draw_rect(&self, dirty_rect: NSRect) {
            if self.ivars().textkit_graph_teardown.get() {
                // 原会话已结束，迟到 dirty 不能通过 super 重新绘制或恢复旧正文载荷。
                return;
            }
            // 画图栈可能暂持可见正文/属性，外部关闭必须等这个同步租约结束。
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            #[cfg(feature = "native-raster-surface-probe")]
            let raster = self.ivars().raster_surface.borrow().clone();
            #[cfg(feature = "native-raster-surface-probe")]
            let overlays_in_raster = raster.is_some();
            #[cfg(not(feature = "native-raster-surface-probe"))]
            let overlays_in_raster = false;
            #[cfg(feature = "native-raster-surface-probe")]
            // 原票早于任何本次 Native 绘制或诊断 getter，回调不能给后继输入重新签发图像。
            let raster_origin = raster.as_ref().map(|owner| owner.begin(self));

            native_markdown_trace! {
                eprintln!(
                    "[probe] draw rect begin manager={} cache={} dirty={dirty_rect:?}",
                    self.textLayoutManager().is_some(),
                    self.ivars().visible_table_layout_cache.borrow().is_some()
                );
                #[cfg(feature = "native-markdown-runtime-probe")]
                crate::native_surface::probe_call_diagnostic::memory_point("body_draw_begin");
            }

            if !overlays_in_raster {
                // 默认路径先完成系统正文；光栅试验把同一 super 调用移到有限 CPU context 内。
                // SAFETY: 主线程活 NSTextView 同步接收匹配 dirty，不建立第二份可变文字模型。
                unsafe {
                    let _: () = msg_send![super(self), drawRect: dirty_rect];
                }
                #[cfg(feature = "native-markdown-runtime-probe")]
                crate::native_surface::probe_call_diagnostic::memory_point("body_super_draw_end");
            }
            #[cfg(feature = "native-raster-surface-probe")]
            {
                if let Some(owner) = raster {
                    let permit = match raster_origin.expect("光栅 owner 与原入口观察同时存在") {
                        Ok(Some(permit)) => permit,
                        Ok(None) => return, // 原动作未就绪或已取消，不把父类返回登记成光栅完成。
                        Err(error) => {
                            eprintln!("native_raster_surface event=paint_origin_failed error={error:?} formal=false");
                            return;
                        }
                    };
                    // 原生 glyph 在显式试验中由同源有限画布实际完成；失败不能登记为已绘制。
                    match owner.paint(self, &permit) {
                        Ok(true) => {} // 同一实际图像已经发布，才允许下方既有完成事件推进。
                        Ok(false) => return, // 未挂窗或未可见时没有实际 glyph 绘制，不能当作完成。
                        Err(error) => {
                            eprintln!("native_raster_surface event=paint_failed error={error:?} formal=false");
                            return; // 有效视口绘制失败保留原诊断，不伪造完成或在本栈重试。
                        }
                    }
                }
            }
            // 只有真实native glyph绘制已经返回，才为MissingLayout开放一次后继准备。
            self.preview_blocks_completed_native_draw();
            if !overlays_in_raster {
                // 默认路径保持原窗口绘制；显式光栅路径已在同票 CPU 画布按相同顺序完成这些层。
                self.draw_visible_markdown_tables(dirty_rect);
                self.draw_source_decorations(dirty_rect);
            }
            // 前一请求可能遇到外层TextKit编辑事务；绘图只排合并任务，不在此栈布局表格。
            self.schedule_visible_table_layout_update();
            native_markdown_trace! {
                eprintln!("[probe] draw rect end manager={}", self.textLayoutManager().is_some());
                #[cfg(feature = "native-markdown-runtime-probe")]
                crate::native_surface::probe_call_diagnostic::memory_point("body_draw_end");
            }
        }

        #[unsafe(method(mouseDown:))]
        /// 实际表格命中进入独立字段；正文指针、拖选和系统接管沿同一原动作权限推进。
        fn mouse_down(&self, event: &NSEvent) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() {
                // 关闭后的系统空壳不命中旧几何，也不恢复文本输入。
                return;
            }
            self.ivars().folds.invalidate_native_install();
            let Ok(capture) = self.ivars().folds.capture_selection() else { return; };
            let input_epoch = self.ivars().table_input_intent_epoch.get();
            let current = || capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch;
            let Ok(window_point) = native_markdown_table_intent_step(&current, || event.locationInWindow()) else { return; };
            let Ok(point) = native_markdown_table_intent_step(&current, || self.convertPoint_fromView(window_point, None)) else { return; };
            let Ok(cell) = native_markdown_table_intent_step(&current, || self.visible_table_cell_at_point(point)) else { return; };
            if !current() {
                return; // 命中查询重入后不能把旧表格坐标交给新的笔记或窗口宽度。
            }
            if let Some(cell) = cell {
                // 表格字段负责自己的提交、焦点和有限行请求；此处不预先提交另一份草稿。
                self.activate_table_cell(cell, event);
                return;
            }
            match self.coordinate_body_mouse_down(event) {
                NativeMarkdownMouseDisposition::Handled => {
                    // 自有选择已经完成或原动作被撤销；不再重复发送系统鼠标动作。
                }
                NativeMarkdownMouseDisposition::NativeOwned(forward) => {
                    // 链接、已有选择的拖放和输入法等保留公开系统交互；临时存储 claim 跨整个 super 栈。
                    forward.if_current(self, || {
                        // SAFETY: 原真实 NSEvent 仍由本主线程回调持有；同一原票只同步转发一次。
                        unsafe { let _: () = msg_send![super(self), mouseDown: event]; }
                    });
                }
            }
        }

        #[unsafe(method(hasMarkedText))]
        /// Reports native transient composition during input and canonical settled composition between callbacks.
        fn has_marked_text(&self) -> bool {
            if self.ivars().textkit_input_depth.get() != 0 {
                // SAFETY: while forwarding an input operation, ask NSTextView
                // for its own transient state so its implementation sees the
                // same state it established internally.
                return unsafe { msg_send![super(self), hasMarkedText] };
            }
            self.ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        }

        #[unsafe(method(markedRange))]
        /// Keeps the current IME span scalar-safe without exposing a second composition text model.
        fn marked_range(&self) -> NSRange {
            if self.ivars().textkit_input_depth.get() != 0 {
                // SAFETY: see `has_marked_text`; this avoids feeding the
                // host's internal TextKit transaction back into our semantic
                // range adapter.
                return unsafe { msg_send![super(self), markedRange] };
            }
            self.ivars()
                .marked_text_range
                .get()
                .unwrap_or_else(|| NSRange::new(NSNotFound as usize, 0))
        }

        #[unsafe(method(selectedRange))]
        /// Uses native transient selection only while a guarded input operation owns its reconciliation.
        fn selected_range(&self) -> NSRange {
            if self.ivars().textkit_graph_teardown.get() {
                // 已封闭客户端没有可编辑文档；公开协议以 NSNotFound 表示无选择，不能暴露旧 caret。
                return NSRange::new(NSNotFound as usize, 0);
            }
            if self.ivars().textkit_input_depth.get() != 0 {
                // Native insertion reconciles a transient selection before canonical metadata settles.
                return self.super_selected_range();
            }
            self.semantic_selection_range()
                .unwrap_or_else(|| self.super_selected_range())
        }

        #[unsafe(method_id(annotatedSubstringForProposedRange:actualRange:))]
        /// 关闭后的客户端不再提供检查文本；活动文档保持系统原有的段落与注解语义。
        ///
        /// # Safety
        /// `actual_range` 为 NULL 或调用方提供的可写 NSRange，且调用发生在所属 AppKit 主线程。
        unsafe fn annotated_substring_for_proposed_range(
            &self,
            range: NSRange,
            actual_range: *mut NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() {
                // 清理 superclass 选择会同步通知文本检查器；NSNotFound 对仍连接的 superclass
                // 表示检查全文。此时业务客户端已经封闭，没有可检查范围，不能再复制旧正文。
                if !actual_range.is_null() {
                    // SAFETY: 非空输出指针遵守本方法的写入合同；不读取调用方未初始化的值。
                    unsafe { actual_range.write(NSRange::new(NSNotFound as usize, 0)); }
                }
                None
            } else {
                #[cfg(all(feature = "native-block-fragment-diagnostic", feature = "native-markdown-runtime-probe"))]
                {
                    use super::frozen_annotation_capture::FrozenAnnotationCapture;
                    match super::frozen_annotation_capture::capture(self, range) {
                        FrozenAnnotationCapture::PassThrough => {
                            // 未显式启用或尚无原owner票时保持同一次super；fallback不冒充bounded成功。
                            // SAFETY: 原公开范围/输出指针不变，只转发存活superclass一次。
                            unsafe {
                                msg_send![super(self), annotatedSubstringForProposedRange: range, actualRange: actual_range]
                            }
                        }
                        FrozenAnnotationCapture::Outside => None, // 完全不相交不写actualRange，保持协议的nil结果。
                        FrozenAnnotationCapture::Cancelled => None, // 原票撤权后只丢本候选，不能取后继权限重做。
                        FrozenAnnotationCapture::Value(value, actual) => {
                            if !actual_range.is_null() {
                                // SAFETY: caller提供可写单一NSRange；只发布已认证同源、完整段落调整的真实范围。
                                unsafe { actual_range.write(actual); }
                            }
                            Some(value) // 单尾method_id按noncopy约定移交原不可变值，不多retain或复制正文。
                        }
                    }
                }
                #[cfg(not(all(feature = "native-block-fragment-diagnostic", feature = "native-markdown-runtime-probe")))]
                {
                    // 普通构建继续由系统提供原段落与注解语义，不加载实验类或改变输入交互。
                    // SAFETY: 原范围和输出指针遵守公开客户端合同，存活superclass同步处理一次。
                    unsafe {
                        msg_send![super(self), annotatedSubstringForProposedRange: range, actualRange: actual_range]
                    }
                }
            }
        }

        #[unsafe(method(insertText:replacementRange:))]
        /// 以原计划接住一次标准输入的真实提交结果，成功后才结束组合状态。
        ///
        /// # Safety
        /// string 必须是本回调期间存活的 NSString 或 NSAttributedString，且由所属 AppKit 主线程调用。
        unsafe fn insert_text_replacement_range(&self, string: &AnyObject, replacement_range: NSRange) {
            // 输入解释或 trace scope 尚在栈内时，资源预检不能接受旧业务结束。
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

            if self.ivars().textkit_graph_teardown.get() {
                // Accepted close seals native composition/selection work as well as canonical edits;
                // forwarding late input could otherwise create new system ownership after the final save.
                return;
            }
            let result = self.run_planned_native_input(string, replacement_range, None, |range| {
                // SAFETY: 原 AppKit 输入对象仍在回调栈；预检后的同一原计划只向 super 转发一次。
                unsafe { let _: () = msg_send![super(self), insertText: string, replacementRange: range]; }
            });
            if let Err(error) = result {
                #[cfg(feature = "native-markdown-runtime-probe")]
                if std::env::var_os("OMNI_NATIVE_PROBE_INPUT_DIAGNOSTIC").is_some()
                    || std::env::var_os("OMNI_NATIVE_PROBE_INPUT_CAPACITY_REJECTION").is_some()
                {
                    // 只记录输入边界产生的域错误，不记录 incoming 字符串或正文。
                    eprintln!("omni_input phase=insert_rejected reason={error}");
                }
                present_native_markdown_error(&error); // 拒绝只提示，不清除或重写原候选。
            }
            self.schedule_visible_table_layout_update();
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        /// 候选及相对选择预检后只提交一次 Core 事务，完整方向与 marked 状态在通知前发布。
        ///
        /// # Safety
        /// string 须为存活的受支持文本；selected_range 相对该候选，replacement_range 遵守显式范围或 NSNotFound 合同。
        unsafe fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            selected_range: NSRange,
            replacement_range: NSRange,
        ) {
            // 输入解释或 trace scope 尚在栈内时，资源预检不能接受旧业务结束。
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

            if self.ivars().textkit_graph_teardown.get() {
                // Native-only provisional state must not restart after the saved foreground input gate closes.
                return;
            }
            let result = self.run_planned_native_input(string, replacement_range, Some(selected_range), |range| {
                // SAFETY: 本次原候选及相对选择已按 scalar 校验，super 只消费同一显式替换范围。
                unsafe {
                    let _: () = msg_send![super(self), setMarkedText: string, selectedRange: selected_range, replacementRange: range];
                }
            });
            if let Err(error) = result {
                #[cfg(feature = "native-markdown-runtime-probe")]
                if std::env::var_os("OMNI_NATIVE_PROBE_INPUT_DIAGNOSTIC").is_some()
                    || std::env::var_os("OMNI_NATIVE_PROBE_INPUT_CAPACITY_REJECTION").is_some()
                {
                    // 拒绝首因与有限阶段一起保留；不能只用最终 marked 状态反猜失败路径。
                    eprintln!("omni_input phase=set_marked_rejected reason={error}");
                }
                present_native_markdown_error(&error); // 没有原 primitive 成功收据就不发布 marked 成功状态。
            }
        }

        #[unsafe(method(unmarkText))]
        /// 原票下解除实际 Native 标记；无成功解除证明时保留组合状态，不重写候选。
        ///
        /// # Safety
        /// view、存储及原输入事务须在所属 AppKit 主线程保持存活。
        unsafe fn unmark_text(&self) {
            // 输入解释或 trace scope 尚在栈内时，资源预检不能接受旧业务结束。
            let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);

            if self.ivars().textkit_graph_teardown.get() {
                // Closing was authorized only after composition cleared; stale selectors cannot accept more edits.
                return;
            }
            // NSTextView must clear its own marked-text bookkeeping as well as our semantic
            // range. Forward under the existing native-input guard so reentrant getters observe
            // superclass state while the standard commit operation is in progress.
            if self.ivars().unmarking_text.replace(true) {
                // A nested superclass callback belongs to the current unmark operation.
                return;
            }
            let result = self.run_native_unmark(|| {
                // SAFETY: 原主线程 NSTextView 仍拥有同一输入图；只解除一次标记，不另发字符修改。
                unsafe { let _: () = msg_send![super(self), unmarkText]; }
            });
            if let Err(error) = result {
                #[cfg(feature = "native-markdown-runtime-probe")]
                if std::env::var_os("OMNI_NATIVE_PROBE_INPUT_DIAGNOSTIC").is_some()
                    || std::env::var_os("OMNI_NATIVE_PROBE_INPUT_CAPACITY_REJECTION").is_some()
                {
                    // unmark 没有字符载荷；输出原同步路径的第一个拒绝原因，保留真实现场。
                    eprintln!("omni_input phase=unmark_rejected reason={error}");
                }
                present_native_markdown_error(&error);
            }
            self.ivars().unmarking_text.set(false);
            self.schedule_visible_table_layout_update();
        }

        #[unsafe(method(insertNewline:))]
        /// Route settled Return through canonical Markdown continuation and ordinary indentation semantics.
        fn insert_newline(&self, sender: Option<&AnyObject>) {
            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }

            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !self.run_canonical_editing_command(MarkdownEditingCommand::InsertNewline)
            {
                // Native composition gets first refusal; an explicitly unhandled context uses normal AppKit input.
                // SAFETY: forward the matching selector and borrowed sender synchronously on the main thread.
                unsafe {
                    let _: () = msg_send![super(self), insertNewline: sender];
                }
            }
        }

        #[unsafe(method(deleteBackward:))]
        /// Remove Markdown block markup first, preserving native grapheme deletion when no command applies.
        fn delete_backward(&self, sender: Option<&AnyObject>) {
            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }

            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !self.run_canonical_editing_command(MarkdownEditingCommand::DeleteMarkupBackward)
            {
                // 这里只标记真实delete动作；composition state使IME内部删除仍沿其输入语义。
                let _delete = self.ivars().folds.begin_delete();
                // SAFETY: the live main-thread superclass receives its original borrowed sender exactly once.
                unsafe {
                    let _: () = msg_send![super(self), deleteBackward: sender];
                }
            }
        }

        #[unsafe(method(deleteForward:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_forward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteForward: sender]; }
        }

        #[unsafe(method(deleteBackwardByDecomposingPreviousCharacter:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_backward_decomposing(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteBackwardByDecomposingPreviousCharacter: sender]; }
        }

        #[unsafe(method(deleteWordForward:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_word_forward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteWordForward: sender]; }
        }

        #[unsafe(method(deleteWordBackward:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_word_backward(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteWordBackward: sender]; }
        }

        #[unsafe(method(deleteToBeginningOfLine:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_to_beginning_of_line(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteToBeginningOfLine: sender]; }
        }

        #[unsafe(method(deleteToEndOfLine:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_to_end_of_line(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteToEndOfLine: sender]; }
        }

        #[unsafe(method(deleteToBeginningOfParagraph:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_to_beginning_of_paragraph(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteToBeginningOfParagraph: sender]; }
        }

        #[unsafe(method(deleteToEndOfParagraph:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_to_end_of_paragraph(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteToEndOfParagraph: sender]; }
        }

        #[unsafe(method(deleteToMark:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_to_mark(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), deleteToMark: sender]; }
        }

        #[unsafe(method(cut:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn cut_source(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), cut: sender]; }
        }

        #[unsafe(method(delete:))]
        /// 既有系统删除动作仅增加纯语义scope，不新增快捷键或改变super处理。
        fn delete_selection(&self, sender: Option<&AnyObject>) {
            let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
            if self.ivars().textkit_graph_teardown.get() { return; }
            let _delete = self.ivars().folds.begin_delete();
            // SAFETY: 原标准selector在同一主线程以原sender转发一次，RAII在super返回后结束。
            unsafe { let _: () = msg_send![super(self), delete: sender]; }
        }

        #[unsafe(method(insertTab:))]
        /// Indent selected source lines with the preserved two-space command; preview keeps native routing.
        fn insert_tab(&self, sender: Option<&AnyObject>) {
            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }

            let source =
                self.ivars().text_storage.load().is_some_and(|storage| {
                    storage.presentation_mode() == MarkdownPresentationMode::Source
                });
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !source
                || !self.run_canonical_editing_command(MarkdownEditingCommand::IndentMore)
            {
                // Non-source/native composition input is handled once by the original responder implementation.
                // SAFETY: forward the matching input selector on the live main-thread text view.
                unsafe {
                    let _: () = msg_send![super(self), insertTab: sender];
                }
            }
        }

        #[unsafe(method(insertBacktab:))]
        /// Outdent source lines without turning a handled no-op into native focus traversal.
        fn insert_backtab(&self, sender: Option<&AnyObject>) {
            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }

            let source =
                self.ivars().text_storage.load().is_some_and(|storage| {
                    storage.presentation_mode() == MarkdownPresentationMode::Source
                });
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !source
                || !self.run_canonical_editing_command(MarkdownEditingCommand::IndentLess)
            {
                // An unclaimed native context keeps its standard Shift-Tab behavior.
                // SAFETY: the live superclass borrows the original sender for this synchronous selector.
                unsafe {
                    let _: () = msg_send![super(self), insertBacktab: sender];
                }
            }
        }

        #[unsafe(method(undo:))]
        /// Routes native Undo to the sole bounded structural piece-tree history.
        fn undo(&self, _sender: Option<&AnyObject>) {
            self.restore_history(MarkdownHistoryDirection::Undo);
        }

        #[unsafe(method(redo:))]
        /// Routes native Redo to structural history without enabling NSUndoManager storage.
        fn redo(&self, _sender: Option<&AnyObject>) {
            self.restore_history(MarkdownHistoryDirection::Redo);
        }

        #[unsafe(method(cancelOperation:))]
        /// Leaves selector-only cancellation with AppKit so input-source actions cannot dismiss the plugin.
        fn cancel_operation(&self, sender: Option<&AnyObject>) {
            if self.ivars().textkit_graph_teardown.get() {
                // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
                return;
            }

            // Input methods can issue cancelOperation: without a physical Escape event. Native
            // composition owns those selectors; window-level physical key routing owns dismissal.
            // SAFETY: the live main-thread superclass receives the original borrowed sender once.
            unsafe {
                let _: () = msg_send![super(self), cancelOperation: sender];
            }
        }

        // 独立 TextKit 参考图不拥有正式窗口关闭权限；窗口级按键继续由实际 panel/responder 负责。

    }
);

impl PieceBackedMarkdownTextView {
    #[cfg(feature = "native-raster-surface-probe")]
    /// 仅供原光栅事务在已安装的有限 CPU context 中执行一次系统背景与选择绘制。
    pub(super) fn draw_native_body_for_raster(&self, dirty: NSRect) {
        if self.ivars().textkit_graph_teardown.get() {
            // 关闭撤销原事务，不能从 helper 另开一条迟到的系统绘图路径。
            return;
        }
        // SAFETY: 调用方持同一 view 的同步 payload 租约和当前 Native 图；只转发匹配 super 方法。
        unsafe {
            let _: () = msg_send![super(self), drawRect: dirty];
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        crate::native_surface::probe_call_diagnostic::memory_point("body_super_draw_end");
    }

    /// 只让真实可见正文失效，避免文档随全文增高后将整个视图标脏并诱发视口外排版。
    /// 返回值只说明失效调用前后尚未关闭，不授予选择、代次或语义 Ready 权限。
    pub(super) fn invalidate_visible_body(&self) -> bool {
        #[cfg(feature = "native-markdown-runtime-probe")]
        let _reentry_invalidate_scope = super::viewport_reentry_diagnostic::enter(
            super::viewport_reentry_diagnostic::Phase::InvalidateBody,
        );
        if self.ivars().textkit_graph_teardown.get() {
            return false; // 已接受关闭的正文不再发起绘制或查询 Native 可见几何。
        }
        let visible = {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::InvalidateClip,
            );
            self.visibleRect()
        };
        let right = visible.origin.x + visible.size.width;
        let bottom = visible.origin.y + visible.size.height;
        if self.ivars().textkit_graph_teardown.get()
            || ![
                visible.origin.x,
                visible.origin.y,
                visible.size.width,
                visible.size.height,
                right,
                bottom,
            ]
            .into_iter()
            .all(f64::is_finite)
            || visible.size.width <= 0.0
            || visible.size.height <= 0.0
            || right <= visible.origin.x
            || bottom <= visible.origin.y
        {
            // getter 可重入关闭；空、异常或端点溢出矩形都不扩成 bounds/full-document 替代请求。
            return false;
        }
        {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::InvalidateDisplay,
            );
            self.setNeedsDisplayInRect(visible);
        }
        // Native setter 后仍只复核纯关闭标量；调用方继续持有并校验其原 native_step/提交权限。
        !self.ivars().textkit_graph_teardown.get()
    }

    /// 原 pending owner 在首 Native 前固定；迟到、未知和重入通知不能领取普通选择的新票。
    fn consume_pending_native_selection(&self, notification: &NSNotification) -> bool {
        let slot = self
            .ivars()
            .pending_selection_slot
            .borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade);
        let Some(slot) = slot else {
            return false;
        };
        let owner = slot.borrow().as_ref().map(Rc::clone);
        let Some(owner) = owner else {
            return false;
        };
        let trace = native_markdown_pending_selection_trace_enabled();
        let Some(_sample) = owner.begin_sample() else {
            if trace {
                eprintln!(
                    "omni_pending event=overlap id={} source={} phase={:?} original_current={}",
                    owner.id,
                    owner.source,
                    owner.phase.get(),
                    owner.committed.current()
                );
            }
            return true; // revoked tombstone 或 reading 重入均已撤旧权，不能执行递归 getter。
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        if !super::probe::pending_fault::NativeMarkdownPendingSelectionFault::before_read(
            self, &owner,
        ) {
            owner.reject();
            return true; // 明确的单次 reader-failure seam，不把它宣称为框架真实 nil。
        }
        let current_before = owner.current();
        let getter = Cell::new("sender");
        let observation = owner.native_step(|| {
            autoreleasepool(|_| {
                let object = owner
                    .native_step(|| notification.object())?
                    .ok_or(NativeMarkdownFoldError::InvalidSelection)?;
                getter.set("sender_type");
                let sender = owner
                    .native_step(|| object.downcast::<NSTextView>().ok())?
                    .ok_or(NativeMarkdownFoldError::InvalidSelection)?;
                if !std::ptr::eq::<NSTextView>(&*sender, self.as_super()) {
                    getter.set("foreign_sender");
                    return Err(NativeMarkdownFoldError::InvalidSelection);
                }
                drop(sender); // sender claim 的析构也受外层同 owner 门禁覆盖。
                getter.set("storage");
                let storage = owner
                    .native_step(|| self.ivars().text_storage.load())?
                    .ok_or(NativeMarkdownFoldError::Stale)?;
                if !Rc::ptr_eq(&storage.ivars().pending_selection, &slot) {
                    return Err(NativeMarkdownFoldError::Stale);
                }
                drop(storage);
                getter.set("selected_ranges");
                let ranges: Option<Retained<NSArray<objc2_foundation::NSValue>>> = owner
                    .native_step(|| {
                        // SAFETY: 原正文 superclass 的 nullable 公开 getter；这里只读 raw，绝不访问对应正文。
                        unsafe { msg_send![super(self), selectedRanges] }
                    })?;
                let present = ranges.is_some();
                getter.set("count");
                let count = match &ranges {
                    Some(ranges) => owner.native_step(|| ranges.count())?,
                    None => 0, // nil 不等于 EOF，更不能伪装成实际 Single。
                };
                getter.set("selected_range");
                let raw = owner.native_step(|| self.super_selected_range())?;
                getter.set("claims_and_pool");
                drop(ranges); // 原票的后检查发生在所有 Native claims 和池释放之后。
                Ok::<_, NativeMarkdownFoldError>((present, count, raw))
            })
        });
        match observation {
            Ok(Ok((present, count, raw))) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                owner.record_read();
                let accepted = owner.observe(present, count, raw);
                if trace {
                    eprintln!(
                        "omni_pending event=delegate id={} source={} phase={:?} present={present} count={count} raw={raw:?} hits={} accepted={accepted} original_before={current_before} original_after={}",
                        owner.id,
                        owner.source,
                        owner.phase.get(),
                        owner.source_hits(raw),
                        owner.current()
                    );
                }
            }
            _ => {
                owner.reject(); // 实际 getter 失败或原票失效均是终态拒绝；不清槽、不重签或补观察。
                if trace {
                    eprintln!(
                        "omni_pending event=unavailable id={} source={} phase={:?} getter={} original_before={current_before} original_after={} getters_complete=false",
                        owner.id,
                        owner.source,
                        owner.phase.get(),
                        getter.get(),
                        owner.current()
                    );
                }
            }
        }
        true // 只要入口持原 owner，就不会再走普通 fresh capture 分支。
    }

    /// callback 只有 Weak backlink；唯一强输入槽仍在 storage，旧 guard 不恢复前任。
    fn install_input_callback<'view>(
        &'view self,
        input: &Rc<NativeMarkdownInputTransaction>,
    ) -> Result<NativeMarkdownInputCallbackGuard<'view>, String> {
        if self.ivars().input_callback.borrow().is_some() || !input.current() {
            return Err("输入回调在安装原弱票前已失效".into());
        }
        let weak = Rc::downgrade(input);
        self.ivars().input_callback.replace(Some(weak.clone()));
        Ok(NativeMarkdownInputCallbackGuard {
            view: self,
            input: weak,
        })
    }

    /// superclass 的子输入只能沿 unmark 原票消费 canonical 等值操作，不能升级成新的字符事务。
    fn run_unmark_bookkeeping(
        &self,
        parent: Rc<NativeMarkdownInputTransaction>,
        string: &AnyObject,
        requested: NSRange,
        relative: Option<NSRange>,
        native: impl FnOnce(NSRange),
    ) -> Result<(), String> {
        let result = (|| {
            let Some(original_marked) = parent.unmark_range() else {
                return Err("字符输入产生了未声明的嵌套输入".into());
            };
            let original = parent.captured.selection;
            let original_range = NSRange::new(
                original.anchor_utf16.min(original.head_utf16),
                original.anchor_utf16.abs_diff(original.head_utf16),
            );
            let current = || parent.current() && !self.ivars().textkit_graph_teardown.get();
            let storage =
                native_markdown_table_intent_step(&current, || self.ivars().text_storage.load())
                    .map_err(|_| "解除标记的原存储查询已撤权")?
                    .ok_or("解除标记的原存储已关闭")?;
            if !storage.input_transaction_is_current(&parent) {
                return Err("解除输入标记时，原输入事务已被替换".into());
            }
            let current = || {
                storage.input_transaction_is_current(&parent)
                    && !self.ivars().textkit_graph_teardown.get()
            };
            let text = copy_native_markdown_input_text(string, &current)?;
            let units = text.encode_utf16().count();
            let range = if requested.location != NSNotFound as usize {
                requested
            } else {
                // sentinel 只能在原 marked 或原有序选择两个已授权范围内解析；实际值不能产生第三范围。
                let marked =
                    native_markdown_table_intent_step(&current, || self.native_marked_state())
                        .map_err(|_| "解除标记的实际候选查询已撤权")?;
                if marked.0 && original_marked == Some(marked.1) {
                    marked.1
                } else if !marked.0 {
                    let actual = self
                        .native_ordered_selection_state_for_capture(
                            parent.captured.native_capture(),
                        )
                        .map_err(|error| error.to_string())?;
                    if actual != NativeMarkdownOrderedSelection::Single(original_range) {
                        return Err("解除输入标记时，选择范围与原输入不一致".into());
                    }
                    original_range
                } else {
                    return Err("解除标记的实际候选不再属于原范围".into());
                }
            };
            let document = storage.document().ok_or("解除标记的原文档已关闭")?;
            let equal = {
                let document = document.borrow();
                native_markdown_input_equals_source(&document, range, &text)?
            };
            drop(document); // 等值证明只保留 bool；不持有临时正文强 root 跨下面的 Native 调用。
            #[cfg(feature = "native-markdown-runtime-probe")]
            if std::env::var_os("OMNI_NATIVE_PROBE_INPUT_DIAGNOSTIC").is_some()
                || std::env::var_os("OMNI_NATIVE_PROBE_INPUT_CAPACITY_REJECTION").is_some()
            {
                eprintln!(
                    "omni_input phase=unmark_child_shape type={} bytes={} units={units} requested={requested:?} resolved={range:?} relative={relative:?} canonical_equal={equal} strict_empty={}",
                    u8::from(relative.is_some()),
                    text.len(),
                    text.is_empty() && range.length == 0
                );
            }
            if relative == Some(NSRange::new(0, 0)) && text.is_empty() {
                // 实测 superclass 先请求删除 marked、再插回原文字；只延迟这一 exact 清除指令。
                // 此处既不转发删除，也不清项目 marked；后继必须提交原文字才可让整个 unmark 成功。
                parent.defer_unmark_clear(range)?;
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_input_probe_stage("unmark_clear_deferred", &storage);
                return Ok(());
            }
            if relative.is_some() {
                return Err("解除标记未收到原协议的空清除请求".into());
            }
            if !equal {
                return Err("解除标记的提交与原正文不等值，已拒绝".into());
            }
            // 第二步只允许 insert，临时 caret 完全由已验证的等值范围与 incoming 长度导出。
            let selected = NSRange::new(
                range
                    .location
                    .checked_add(units)
                    .ok_or("解除输入标记后的光标位置超出可表示范围")?,
                0,
            );
            let install = parent
                .captured
                .begin_bookkeeping_install(selected)
                .map_err(|error| error.to_string())?;
            let child = parent.begin_bookkeeping(range, text)?;
            self.begin_textkit_input_operation();
            if current() && install.current() {
                native(range);
            } else {
                parent.reject("解除标记子调用在进入系统前已撤权");
            }
            self.end_textkit_input_operation();
            if !current() || !install.current() || parent.failure().is_some() {
                return Err(parent
                    .failure()
                    .unwrap_or_else(|| "解除标记子调用结束时原票已失效".into()));
            }
            // 只恢复首 Native 前已声明的原完整选择；没有 Core 变化，也不发布新的 serial。
            let affinity = if original.head_utf16 < original.anchor_utf16 {
                objc2_app_kit::NSSelectionAffinity::Upstream
            } else {
                objc2_app_kit::NSSelectionAffinity::Downstream
            };
            native_markdown_table_intent_step(&current, || {
                // SAFETY: 等值字符操作未改长度，原完整选择仍为 canonical scalar 边界；原 lease 仅准许两范围。
                unsafe { let _: () = msg_send![super(self), setSelectedRange: original_range, affinity: affinity, stillSelecting: false]; }
            }).map_err(|_| "解除标记恢复原范围时已撤权")?;
            let actual = self
                .native_ordered_selection_state_for_capture(parent.captured.native_capture())
                .map_err(|error| error.to_string())?;
            if actual != NativeMarkdownOrderedSelection::Single(original_range)
                || !current()
                || !install.current()
                || self.ivars().folds.known_selection() != Some(original)
            {
                return Err("解除标记的等值子调用未恢复原完整选择".into());
            }
            parent.complete_unmark_bookkeeping(&child.child)?;
            drop(child);
            drop(install);
            Ok(())
        })();
        if let Err(error) = &result {
            parent.reject(error);
        }
        result
    }

    /// 用原输入票预检候选；只复制本次新增输入，不读取或保存原文字符串。
    fn prepare_native_input_transaction(
        &self,
        string: &AnyObject,
        requested: NSRange,
        relative: Option<NSRange>,
    ) -> Result<
        (
            Retained<PieceBackedMarkdownTextStorage>,
            Rc<NativeMarkdownInputTransaction>,
        ),
        String,
    > {
        let capture = self
            .ivars()
            .folds
            .capture_selection()
            .map_err(|error| error.to_string())?;
        let storage = selection_native_step(&capture, || self.ivars().text_storage.load())
            .map_err(|error| error.to_string())?
            .ok_or("笔记输入存储已关闭")?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage(
            if relative.is_some() {
                "prepare_marked"
            } else {
                "prepare_insert"
            },
            &storage,
        );
        let epoch = storage.reserve_input_transaction_epoch()?;
        let current = || {
            capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && storage.ivars().input_transaction_epoch.get() == epoch
        };
        // 字符串转换只拥有 incoming，整个 Native 访问与池结束仍由原 capture 认证。
        let text = copy_native_markdown_input_text(string, &current)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("prepare_after_text", &storage);
        let document = storage.document().ok_or("笔记文档已关闭")?;
        let before = document.borrow().selection();
        let known = self.ivars().folds.known_selection() == Some(before);
        let range = if requested.location != NSNotFound as usize {
            requested // 显式无效范围必须拒绝，不能悄悄替换旧候选或当前选择。
        } else if let Some(marked) = self.ivars().marked_text_range.get() {
            marked
        } else if known {
            NSRange::new(
                before.anchor_utf16.min(before.head_utf16),
                before.anchor_utf16.abs_diff(before.head_utf16),
            )
        } else {
            // 这里只读取真实有序替换范围，不以它猜测非空选择的方向。
            match native_markdown_table_intent_step(&current, || {
                self.native_ordered_selection_state_for_capture(&capture)
            })
            .map_err(|_| "读取输入替换范围时原票已失效")?
            .map_err(|error| error.to_string())?
            {
                NativeMarkdownOrderedSelection::Single(range) => range,
                NativeMarkdownOrderedSelection::Multiple => {
                    return Err("多重选择没有唯一输入替换范围".into());
                }
            }
        };
        let end = range
            .location
            .checked_add(range.length)
            .ok_or("输入替换范围溢出")?;
        {
            let document = document.borrow();
            // 显式范围和旧 marked 范围都须为真实 scalar 边界；低 surrogate 直接在 super 前拒绝。
            document
                .byte_offset_for_utf16(range.location)
                .map_err(|error| error.to_string())?;
            document
                .byte_offset_for_utf16(end)
                .map_err(|error| error.to_string())?;
        }
        let length = text.encode_utf16().count();
        let replacement_end = range.location.checked_add(length).ok_or("候选范围溢出")?;
        let after = if let Some(selected) = relative {
            let selected_end = selected
                .location
                .checked_add(selected.length)
                .ok_or("候选选择范围溢出")?;
            if selected_end > length
                || !input_text_has_utf16_boundary(&text, selected.location)
                || !input_text_has_utf16_boundary(&text, selected_end)
            {
                return Err("候选选择不在有效的 Unicode 标量边界".into());
            }
            MarkdownTextSelection {
                anchor_utf16: range
                    .location
                    .checked_add(selected.location)
                    .ok_or("候选选择位置溢出")?,
                head_utf16: range
                    .location
                    .checked_add(selected_end)
                    .ok_or("候选选择位置溢出")?,
            }
        } else {
            MarkdownTextSelection::caret(replacement_end)
        };
        if !current() {
            return Err("输入候选在校验期间已失效".into());
        }
        let captured = capture
            .finish(before, known)
            .map_err(|error| error.to_string())?;
        let marked_after =
            (relative.is_some() && length != 0).then_some(NSRange::new(range.location, length));
        let transaction = Rc::new(NativeMarkdownInputTransaction::new(
            captured,
            epoch,
            range,
            text,
            after,
            marked_after,
            Rc::clone(&self.ivars().marked_text_range),
        ));
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("prepare_ready", &storage);
        Ok((storage, transaction))
    }

    /// super 仍执行一次真实输入；唯一 primitive 的原 Rc 收据决定成功，长度不再作为接受证明。
    fn run_planned_native_input(
        &self,
        string: &AnyObject,
        requested: NSRange,
        relative: Option<NSRange>,
        native: impl FnOnce(NSRange),
    ) -> Result<(), String> {
        let parent = self
            .ivars()
            .input_callback
            .borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade);
        if let Some(parent) = parent {
            return self.run_unmark_bookkeeping(parent, string, requested, relative, native);
        }
        let (storage, transaction) =
            self.prepare_native_input_transaction(string, requested, relative)?;
        let guard = storage.install_input_transaction(Rc::clone(&transaction))?;
        // 所有 fallible Core 工作都先于 super；容量、追加、observer 或 journal 失败时不启动 Native 编辑。
        if let Err(error) = storage.prepare_input_transaction(&transaction) {
            transaction.reject(&error);
            return Err(error);
        }
        let callback = self.install_input_callback(&transaction)?;
        let was_composing = storage.ivars().composition_active.get();
        if relative.is_some() {
            storage.begin_composition(); // 失败时只还原本次打开的组合标量，原 marked Cell 尚未改变。
        }
        self.begin_textkit_input_operation();
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("planned_before_super", &storage);
        if storage.input_transaction_is_current(&transaction) {
            native(transaction.range);
        } else {
            transaction.reject("原输入在进入系统编辑前已撤销");
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("planned_after_super", &storage);
        #[cfg(feature = "native-markdown-runtime-probe")]
        super::probe::pending_fault::NativeMarkdownPendingSelectionFault::inject_after_super(
            self,
            &storage,
            &transaction,
        );
        let committed = transaction.committed();
        if committed.is_none() && storage.input_transaction_is_current(&transaction) {
            storage.set_input_composition_state(was_composing);
        }
        // 仍沿同一提交票覆盖延迟属性通知；不能在 super 返回后重新捕获当前选择来续权。
        let notify_guard = committed
            .as_ref()
            .and_then(|receipt| receipt.begin_pre_notify_install().ok());
        if let Some(committed) = &committed {
            storage.pending_selection_phase_for(
                committed,
                NativeMarkdownPendingSelectionPhase::Attributes,
            );
        }
        self.end_textkit_input_operation();
        if let Some(committed) = &committed {
            storage.pending_selection_phase_for(
                committed,
                NativeMarkdownPendingSelectionPhase::SuperclassTail,
            );
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("planned_after_end_input", &storage);
        let failure = transaction.failure();
        let current = storage.input_transaction_is_current(&transaction);
        drop(notify_guard);
        drop(callback);
        if let Some(error) = failure {
            return Err(error);
        }
        let committed = committed.ok_or("系统输入未完成原声明的文本提交")?;
        if !current || !committed.current() {
            return Err("文本已经提交，但后继输入撤销了旧显示动作".into());
        }
        // super及合并属性通知可以留下原事务已声明的EOF暂态；它不是最终caret。
        // 必须趁原输入guard和同Rc的pending收据仍在，完成一次原after安装；缺槽或
        // foreign范围直接失败，不回落普通安装、不从实际值补来源或重新捕获选择。
        let pending = storage
            .pending_selection_for(&committed)
            .ok_or("已提交输入缺少原选择安装收据")?;
        if !self.apply_pending_committed_selection(
            &pending,
            &storage.ivars().pending_selection,
            &|| storage.input_transaction_is_current(&transaction),
        ) {
            return Err("已提交输入无法完成原完整选择安装".into());
        }
        // 安装器已经核原owner、实际单范围和完整方向；这里仍保留原调用栈的exact-after复核。
        let actual = committed
            .native_step(|| self.native_ordered_selection_range())
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let expected = NSRange::new(
            transaction
                .after
                .anchor_utf16
                .min(transaction.after.head_utf16),
            transaction
                .after
                .anchor_utf16
                .abs_diff(transaction.after.head_utf16),
        );
        if actual != expected || self.ivars().folds.known_selection() != Some(transaction.after) {
            return Err("已提交候选的实际选择与原完整选择不一致".into());
        }
        committed
            .native_step(|| self.schedule_visible_table_layout_update())
            .map_err(|error| error.to_string())?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        storage
            .ivars()
            .input_callback_success_count
            .set(storage.ivars().input_callback_success_count.get() + 1);
        drop(guard); // 最后一个实际 Native 后果完成后，只清同 Rc 输入和 pending 槽。
        Ok(())
    }

    /// unmark 不授权字符修改；嵌套调用只让原输入计划负责最终 marked 发布。
    fn run_native_unmark(&self, native: impl FnOnce()) -> Result<(), String> {
        let parent = self
            .ivars()
            .input_callback
            .borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade);
        if let Some(parent) = parent {
            // 首个 Native getter 前就持有原 parent；同步子调用不捕获较新的 Core/serial。
            let current = || parent.current() && !self.ivars().textkit_graph_teardown.get();
            let storage =
                native_markdown_table_intent_step(&current, || self.ivars().text_storage.load())
                    .map_err(|_| "嵌套取消标记的原存储查询已撤权")?
                    .ok_or("嵌套取消标记的存储已关闭")?;
            if !storage.input_transaction_is_current(&parent) {
                return Err("嵌套取消标记的原槽已被替换".into());
            }
            self.begin_textkit_input_operation();
            if storage.input_transaction_is_current(&parent) {
                native();
            }
            self.end_textkit_input_operation();
            return storage
                .input_transaction_is_current(&parent)
                .then_some(())
                .ok_or_else(|| "组合输入在同步取消标记时已失效".into());
        }
        let capture = self
            .ivars()
            .folds
            .capture_selection()
            .map_err(|error| error.to_string())?;
        let storage = selection_native_step(&capture, || self.ivars().text_storage.load())
            .map_err(|error| error.to_string())?
            .ok_or("笔记输入存储已关闭")?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("unmark_prepare", &storage);
        if storage.ivars().input_transaction.borrow().is_some() {
            return Err("取消标记没有占用中输入的原弱票".into());
        }
        let epoch = storage.reserve_input_transaction_epoch()?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("unmark_reserved", &storage);
        let document = storage.document().ok_or("笔记文档已关闭")?;
        let before = document.borrow().selection();
        let core_before = {
            let document = document.borrow();
            // 同一不可变piece root只能通过递增generation的Core事务改变；解除标记不提交字符。
            // 保留原代次/长度/历史/完整选择事实即可，不能在每次unmark时流式hash整篇两遍。
            (
                document.generation(),
                document.byte_len(),
                document.utf16_len(),
                document.is_dirty(),
                document.can_undo(),
                document.can_redo(),
                document.selection(),
                document.has_prepared_text_input(),
            )
        };
        let known_before = self.ivars().folds.known_selection();
        #[cfg(feature = "native-markdown-runtime-probe")]
        let counts_before = [
            storage.ivars().character_process_count.get(),
            storage.ivars().input_commit_count.get(),
            storage.ivars().input_callback_success_count.get(),
            storage.ivars().autosave_enqueue_count.get(),
        ];
        let captured = capture
            .finish(before, self.ivars().folds.known_selection() == Some(before))
            .map_err(|error| error.to_string())?;
        drop(document); // 原事实只保留标量；不将临时 Doc 强 owner 带入 superclass。
        // NSNotFound 是故意不可消费的 primitive 范围：本次只允许原生解除标记。
        let transaction = Rc::new(NativeMarkdownInputTransaction::unmark(
            captured,
            epoch,
            Rc::clone(&self.ivars().marked_text_range),
        ));
        let guard = storage.install_input_transaction(Rc::clone(&transaction))?;
        let _callback = self.install_input_callback(&transaction)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("unmark_installed", &storage);
        self.begin_textkit_input_operation();
        if storage.input_transaction_is_current(&transaction) {
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_input_probe_stage("unmark_before_super", &storage);
            native();
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_input_probe_stage("unmark_after_super", &storage);
        } else {
            transaction.reject("取消标记在进入系统调用前已撤权");
        }
        self.end_textkit_input_operation();
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_input_probe_stage("unmark_after_end_input", &storage);
        let current = || storage.input_transaction_is_current(&transaction);
        let native_state = (|| {
            let marked: bool = native_markdown_table_intent_step(&current, || {
                // SAFETY: 活输入图上的公开 superclass getter；前后均守同一个原输入槽。
                unsafe { msg_send![super(self), hasMarkedText] }
            })
            .ok()?;
            let range: NSRange = native_markdown_table_intent_step(&current, || {
                // SAFETY: 相同原票下只读取 superclass 自有 marked 范围，不写回 Core。
                unsafe { msg_send![super(self), markedRange] }
            })
            .ok()?;
            Some((marked, range))
        })();
        if let Some(error) = transaction.failure() {
            return Err(error); // 保留第一个真实 primitive 拒绝原因，不让通用 marked 断言覆盖首因。
        }
        if !storage.input_transaction_is_current(&transaction)
            || !transaction.unmark_is_complete()
            || native_state.is_none_or(|state| {
                state.0 || state.1.location != NSNotFound as usize && state.1.length != 0
            })
        {
            return Err("系统尚未完成当前候选的取消标记".into());
        }
        let expected = NSRange::new(
            before.anchor_utf16.min(before.head_utf16),
            before.anchor_utf16.abs_diff(before.head_utf16),
        );
        let actual = self
            .native_ordered_selection_state_for_capture(transaction.captured.native_capture())
            .map_err(|error| error.to_string())?;
        let core_after = {
            let document = storage.document().ok_or("解除标记后原文档已关闭")?;
            let document = document.borrow();
            // 原事务和原generation联合认证未写正文；不重新捕获新root或把hash失败当相等。
            (
                document.generation(),
                document.byte_len(),
                document.utf16_len(),
                document.is_dirty(),
                document.can_undo(),
                document.can_redo(),
                document.selection(),
                document.has_prepared_text_input(),
            )
        };
        if core_before != core_after
            || actual != NativeMarkdownOrderedSelection::Single(expected)
            || self.ivars().folds.known_selection() != known_before
            || !storage.input_transaction_is_current(&transaction)
            || transaction.failure().is_some()
        {
            return Err("解除标记改变了原正文、历史或完整选择".into());
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        if counts_before
            != [
                storage.ivars().character_process_count.get(),
                storage.ivars().input_commit_count.get(),
                storage.ivars().input_callback_success_count.get(),
                storage.ivars().autosave_enqueue_count.get(),
            ]
        {
            return Err("解除标记产生了额外字符通知、事务或保存动作".into());
        }
        // 未产生字符事务，原 Core 全方向保持；仅在原票仍有效时清除组合标量并 flush 属性。
        self.ivars().marked_text_range.set(None);
        storage.finish_composition();
        let current = storage.input_transaction_is_current(&transaction);
        drop(guard);
        current
            .then_some(())
            .ok_or_else(|| "取消标记后的显示动作已失效".into())
    }

    /// Execute one shared keyboard transaction after visible cell/composition ownership has settled.
    fn run_canonical_editing_command(&self, command: MarkdownEditingCommand) -> bool {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let pending_tail = RefCell::new(None); // 早于所有 Native claims 声明，尾声的原 owner 最后才释放。
        if self.ivars().textkit_graph_teardown.get() {
            // Consume late native commands without allowing fallback to change the sealed storage.
            return true;
        }
        if !self.commit_cell_before_command() {
            // A failed visible-cell commit consumes/vetoes the action instead of falling back and losing its draft.
            return true;
        }
        if !self.commit_composition_before_command() {
            // Markup/indentation must not replace a native provisional range while the IME still owns it.
            return true;
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            // A disconnected old native shell cannot edit a new session through fallback.
            return true;
        };
        let selection = match self.selection_for_command() {
            Ok(selection) => selection,
            Err(error) => {
                present_native_markdown_error(&error.to_string());
                return true;
            }
        };
        let _delete = (command == MarkdownEditingCommand::DeleteMarkupBackward)
            .then(|| self.ivars().folds.begin_delete());
        match storage.apply_editing_command_captured(command, selection) {
            // No applicable markup rule leaves ordinary native input/grapheme deletion available.
            Ok((MarkdownEditingOutcome::NotHandled, _, _pending)) => false,
            // An unindented Shift-Tab is consumed without changing history or native focus.
            Ok((MarkdownEditingOutcome::Unchanged, _, _pending)) => true,
            // The complete receipt is authoritative after canonical borrows and storage notifications have ended.
            Ok((MarkdownEditingOutcome::Changed(_edit), committed, pending)) => {
                drop(pending_tail.replace(pending));
                if self.apply_committed_selection(&committed) {
                    let _ = committed.native_step(|| self.schedule_visible_table_layout_update());
                } // 后继编辑撤票时只停止旧UI后果，已提交正文不会重放或回滚。
                true
            }
            // Rejections keep the exact draft/source/history and stop this action before native fallback can mutate it.
            Err(error) => {
                present_native_markdown_error(&error);
                true
            }
        }
    }

    /// Resolves a physical drop through TextKit 2 selection navigation within the visible container.
    pub(super) fn insertion_offset_at_view_point(&self, point: NSPoint) -> Option<usize> {
        let document_length = self.document_length();
        if document_length == 0 {
            // An empty canonical document has one valid insertion boundary and needs no fragment lookup.
            return Some(0);
        }
        let manager = self.textLayoutManager()?;
        let content = manager.textContentManager()?;
        if content.hasEditingTransaction() {
            // Pointer hit testing must not enumerate text elements inside an active native editing group.
            return None;
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let start = provider.documentRange().location();
        let origin = self.textContainerOrigin();
        let mut bounds = self.visibleRect();
        bounds.origin.x -= origin.x;
        bounds.origin.y -= origin.y;
        // TextKit consumes container coordinates; no deprecated layoutManager/point-index accessor
        // may switch this view into TextKit 1 or build a complete document-sized native text mirror.
        let selections = manager.textSelectionNavigation()
            .textSelectionsInteractingAtPoint_inContainerAtLocation_anchors_modifiers_selecting_bounds(
                NSPoint::new(point.x - origin.x, point.y - origin.y), &start, &NSArray::new(),
                objc2_app_kit::NSTextSelectionNavigationModifier::empty(), false, bounds,
            );
        let range = selections.firstObject()?.textRanges().firstObject()?;
        let offset =
            usize::try_from(provider.offsetFromLocation_toLocation(&start, &range.location()))
                .ok()?;
        // Revalidate the native coordinate against the sole canonical scalar boundary before mutation.
        let storage = self.ivars().text_storage.load()?;
        let document = storage.document()?;
        let valid = document.borrow().byte_offset_for_utf16(offset).is_ok();
        valid.then_some(offset)
    }

    /// Reads canonical UTF-16 length through weak storage without retaining text across native calls.
    pub(super) fn document_length(&self) -> usize {
        self.ivars()
            .text_storage
            .load()
            .and_then(|storage| {
                storage
                    .document()
                    .map(|document| document.borrow().utf16_len())
            })
            .unwrap_or(0)
    }

    /// Starts balanced native getter/style guards before forwarding a standard NSTextInputClient operation.
    pub(super) fn begin_textkit_input_operation(&self) {
        // The storage shares this scope so selection-dependent style invalidation cannot reenter
        // native element enumeration while NSTextView is still inserting or committing marked text.
        if let Some(storage) = self.ivars().text_storage.load() {
            // Share the outer input boundary with storage so derived attribute notifications wait for native insertion.
            storage.begin_native_input();
        }
        let depth = self.ivars().textkit_input_depth.get();
        self.ivars()
            .textkit_input_depth
            .set(depth.saturating_add(1));
    }

    /// Ends native input guards and flushes coalesced derived attributes after superclass input returns.
    pub(super) fn end_textkit_input_operation(&self) {
        let depth = self.ivars().textkit_input_depth.get();
        self.ivars()
            .textkit_input_depth
            .set(depth.saturating_sub(1));
        // End the native view's guard before style notifications can synchronously reenter getters.
        if let Some(storage) = self.ivars().text_storage.load() {
            // Only the final balanced outer input return may publish coalesced presentation invalidation.
            storage.end_native_input();
        }
        if self.ivars().textkit_input_depth.get() == 0 {
            self.finish_fold_layout_update();
        }
    }

    /// Reads NSTextView native selection during its own synchronous input transaction.
    pub(super) fn super_selected_range(&self) -> NSRange {
        // SAFETY: `selectedRange` is an NSTextView primitive and the superclass
        // implementation remains available while this subclass is alive.
        unsafe { msg_send![super(self), selectedRange] }
    }

    /// TextKit 仍连接时清除 superclass 选择；不能等 container 已断开才发送无效 setter。
    pub(super) fn clear_native_selection_before_release(&self) {
        // SAFETY: accepted owner 在主线程仍拥有完整 TextKit 图；0 是任意当前文档的合法起始边界。
        // sealed delegate 不会把这次清理写回 canonical selection/history，也不接受新输入。
        unsafe {
            let _: () = msg_send![super(self), setSelectedRange: NSRange::new(0, 0)];
        }
    }

    /// 只返回实际单一有序范围；调用者不能据此猜测非空选择的 anchor/head。
    pub(super) fn native_ordered_selection_range(
        &self,
    ) -> Result<NSRange, NativeMarkdownFoldError> {
        let capture = self.ivars().folds.capture_selection()?;
        self.native_ordered_selection_for_capture(&capture)
    }

    /// 显式新目标可以收拢实际多选；此查询只观察状态，不授予旧多选的完整 head 权限。
    pub(super) fn native_ordered_selection_state(
        &self,
    ) -> Result<NativeMarkdownOrderedSelection, NativeMarkdownFoldError> {
        let capture = self.ivars().folds.capture_selection()?;
        self.native_ordered_selection_state_for_capture(&capture)
    }

    /// 原票跨全部 Native getter 和临时对象析构保持，供通知与安装收据共用范围认证。
    fn native_ordered_selection_for_capture(
        &self,
        capture: &NativeMarkdownSelectionCapture,
    ) -> Result<NSRange, NativeMarkdownFoldError> {
        match self.native_ordered_selection_state_for_capture(capture)? {
            NativeMarkdownOrderedSelection::Single(range) => Ok(range),
            NativeMarkdownOrderedSelection::Multiple => {
                Err(NativeMarkdownFoldError::InvalidSelection)
            }
        }
    }

    /// 原 capture 固定所有 Native 读取；Multiple 不枚举或物化全部范围，也不读取任意主端点。
    pub(super) fn capture_ordered_native_before(
        &self,
        capture: NativeMarkdownSelectionCapture,
        storage: &PieceBackedMarkdownTextStorage,
        is_current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownCapturedSelection, String> {
        // 原动作票与原选择票共同跨 getter；实际 Single 只提供 ordered before，不能补逻辑 head。
        let observed = native_markdown_table_intent_step(is_current, || {
            self.native_ordered_selection_state_for_capture(&capture)
        })
        .map_err(|_| "原正文选择意图已经改变")?
        .map_err(|error| error.to_string())?;
        let NativeMarkdownOrderedSelection::Single(before) = observed else {
            return Err("当前操作需要单一正文选择".into());
        };
        let document = storage.document().ok_or("原正文已关闭")?;
        if !self.ivars().folds.matches_document(&document) || !capture.current() || !is_current() {
            return Err("原正文选择权限已经改变".into());
        }
        let selection = document.borrow().selection();
        capture
            .finish(selection, false)
            .and_then(|captured| captured.with_observed_native_before(before))
            .map_err(|error| error.to_string())
    }

    /// 原 capture 固定所有 Native 读取；Multiple 不枚举或物化全部范围，也不读取任意主端点。
    pub(super) fn native_ordered_selection_state_for_capture(
        &self,
        capture: &NativeMarkdownSelectionCapture,
    ) -> Result<NativeMarkdownOrderedSelection, NativeMarkdownFoldError> {
        let ranges: Option<Retained<NSArray<objc2_foundation::NSValue>>> =
            selection_native_step(capture, || {
                // SAFETY: 公开 nullable NSArray getter，保持 superclass 的真实对象与所有权 ABI。
                unsafe { msg_send![super(self), selectedRanges] }
            })?;
        let count = match &ranges {
            Some(ranges) => selection_native_step(capture, || ranges.count())?,
            None => 0, // 没有真实范围数组时不把默认位置误认作单 caret。
        };
        if count > 1 {
            drop(ranges);
            return if capture.current() {
                Ok(NativeMarkdownOrderedSelection::Multiple)
            } else {
                Err(NativeMarkdownFoldError::Stale)
            };
        }
        if count == 0 {
            return Err(NativeMarkdownFoldError::InvalidSelection);
        }
        let range = selection_native_step(capture, || self.super_selected_range())?;
        let storage = selection_native_step(capture, || self.ivars().text_storage.load())?
            .ok_or(NativeMarkdownFoldError::Stale)?;
        let document = storage.document().ok_or(NativeMarkdownFoldError::Stale)?;
        if !self.ivars().folds.matches_document(&document) {
            return Err(NativeMarkdownFoldError::Stale);
        }
        {
            let document = document.borrow();
            let end = range
                .location
                .checked_add(range.length)
                .ok_or(NativeMarkdownFoldError::InvalidSelection)?;
            document.byte_offset_for_utf16(range.location)?;
            document.byte_offset_for_utf16(end)?;
        }
        drop(document);
        drop(storage);
        drop(ranges);
        if !capture.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        Ok(NativeMarkdownOrderedSelection::Single(range))
    }

    /// 真实单范围必须与已知 Core 方向一致；桥接 affinity 不再被当作非空选择方向的证明。
    pub(super) fn primary_text_selection(
        &self,
    ) -> Result<NativeMarkdownCapturedSelection, NativeMarkdownFoldError> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let capture = self.ivars().folds.capture_selection()?;
        let view_range = match self.native_ordered_selection_state_for_capture(&capture)? {
            NativeMarkdownOrderedSelection::Single(range) => range,
            NativeMarkdownOrderedSelection::Multiple => {
                return Err(NativeMarkdownFoldError::UnresolvedSelection(capture));
            }
        };
        // 只升级当前view预先登记的weak图，不调用兼容layoutManager或主动请求layout。
        let storage = selection_native_step(&capture, || self.ivars().text_storage.load())?
            .ok_or(NativeMarkdownFoldError::Stale)?;
        let document = storage.document().ok_or(NativeMarkdownFoldError::Stale)?;
        if !self.ivars().folds.matches_document(&document) {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let manager = selection_native_step(&capture, || {
            self.ivars().text_layout_manager.get().and_then(Weak::load)
        })?
        .ok_or(NativeMarkdownFoldError::Stale)?;
        let selections: Option<Retained<NSArray<objc2_app_kit::NSTextSelection>>> =
            selection_native_step(&capture, || {
                // SAFETY: 仍是公开textSelections原getter及对象返回ABI；实测初始focus可返回nil。
                // Option只接收真实nil，不调用会将nil当违反nonnull约束而panic的生成封装。
                unsafe { msg_send![&*manager, textSelections] }
            })?;
        let Some(selections) = selections else {
            return self.initial_native_caret(capture, &document);
        };
        let count = selection_native_step(&capture, || selections.count())?;
        if count == 0 {
            // TK2尚未发布selection时，仅允许公开view信息能证明的单caret，绝不猜非空方向。
            drop(selections);
            return self.initial_native_caret(capture, &document);
        }
        if count != 1 {
            // SDK没有多selection的main字段；把原票据交兼容策略，不重新领取较新代次。
            return Err(NativeMarkdownFoldError::UnresolvedSelection(capture));
        }
        let selection = selection_native_step(&capture, || selections.objectAtIndex(0))?;
        let secondary = selection_native_step(&capture, || selection.secondarySelectionLocation())?;
        if secondary.is_some() {
            // 双向边界的实际插入点还取决于活输入方向；不能从升序range或固定LTR猜测。
            return Err(NativeMarkdownFoldError::UnresolvedSelection(capture));
        }
        let ranges = selection_native_step(&capture, || selection.textRanges())?;
        let count = selection_native_step(&capture, || ranges.count())?;
        if count != 1 {
            return Err(NativeMarkdownFoldError::UnresolvedSelection(capture));
        }
        let range = selection_native_step(&capture, || ranges.objectAtIndex(0))?;
        // 由公开内容provider把逻辑location转为全局UTF16，不从屏幕x/y推测文本anchor。
        let content = selection_native_step(&capture, || manager.textContentManager())?
            .ok_or(NativeMarkdownFoldError::Stale)?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range = selection_native_step(&capture, || provider.documentRange())?;
        let origin = selection_native_step(&capture, || document_range.location())?;
        let start_location = selection_native_step(&capture, || range.location())?;
        let start = selection_native_step(&capture, || {
            content.offsetFromLocation_toLocation(&origin, &start_location)
        })?;
        let end_location = selection_native_step(&capture, || range.endLocation())?;
        let end = selection_native_step(&capture, || {
            content.offsetFromLocation_toLocation(&origin, &end_location)
        })?;
        let start =
            usize::try_from(start).map_err(|_| NativeMarkdownFoldError::InvalidSelection)?;
        let end = usize::try_from(end).map_err(|_| NativeMarkdownFoldError::InvalidSelection)?;
        let document = document.borrow();
        document.byte_offset_for_utf16(start)?;
        document.byte_offset_for_utf16(end)?;
        if start > end || view_range != NSRange::new(start, end - start) {
            return Err(NativeMarkdownFoldError::UnresolvedSelection(capture));
        }
        let known = self.ivars().folds.known_selection().filter(|known| {
            known.anchor_utf16.min(known.head_utf16) == start
                && known.anchor_utf16.max(known.head_utf16) == end
                && document.selection() == *known
        });
        drop(document);
        let oriented = if let Some(known) = known {
            known
        } else if start == end {
            MarkdownTextSelection::caret(start)
        } else {
            // 同一个有序范围可以对应两个逻辑方向；未知非空选择保持显示，不能猜 head 展开折叠。
            return Err(NativeMarkdownFoldError::UnresolvedSelection(capture));
        };
        let head_side = if start == end {
            let affinity = selection_native_step(&capture, || self.selectionAffinity())?;
            if affinity == objc2_app_kit::NSSelectionAffinity::Upstream {
                Some(true)
            } else if affinity == objc2_app_kit::NSSelectionAffinity::Downstream {
                Some(false)
            } else {
                return Err(NativeMarkdownFoldError::InvalidSelection);
            }
        } else {
            self.ivars().folds.known_head_is_upstream()
        };
        // Native局部对象在返回时析构；caller消费capture前再纯核，杜绝析构重入造成旧结果复活。
        capture
            .finish(oriented, true)
            .map(|captured| captured.with_head_side(head_side))
    }

    /// 初始TK2选择可能尚未建立；原view须同时证明只有一个范围且其长度为0。
    fn initial_native_caret(
        &self,
        capture: NativeMarkdownSelectionCapture,
        document: &SharedMarkdownDocument,
    ) -> Result<NativeMarkdownCapturedSelection, NativeMarkdownFoldError> {
        let ranges: Option<Retained<NSArray<objc2_foundation::NSValue>>> =
            selection_native_step(&capture, || {
                // SAFETY: 公开super selectedRanges返回NSArray对象，nullable接收不改变selector/所有权ABI。
                unsafe { msg_send![super(self), selectedRanges] }
            })?;
        let count = match &ranges {
            Some(ranges) => selection_native_step(&capture, || ranges.count())?,
            None => 0, // 没有真实单范围证据，不能把默认0误认作实际caret。
        };
        let range = selection_native_step(&capture, || self.super_selected_range())?;
        drop(ranges); // 临时Native析构也可能重入，完成前不以新票替代原capture。
        if !capture.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let Some(selection) = single_native_caret(count, range) else {
            return Err(NativeMarkdownFoldError::UnresolvedSelection(capture));
        };
        document
            .borrow()
            .byte_offset_for_utf16(selection.head_utf16)?;
        let affinity = selection_native_step(&capture, || self.selectionAffinity())?;
        let head_side = if affinity == objc2_app_kit::NSSelectionAffinity::Upstream {
            Some(true)
        } else if affinity == objc2_app_kit::NSSelectionAffinity::Downstream {
            Some(false)
        } else {
            return Err(NativeMarkdownFoldError::InvalidSelection);
        };
        capture
            .finish(selection, true)
            .map(|captured| captured.with_head_side(head_side))
    }

    /// 无活fold时维持旧多选行为；原capture跨兼容getter保持，不把未知main伪装为已证明方向。
    pub(super) fn selection_for_command(
        &self,
    ) -> Result<NativeMarkdownCapturedSelection, NativeMarkdownFoldError> {
        match self.primary_text_selection() {
            Err(NativeMarkdownFoldError::UnresolvedSelection(capture))
                if self.ivars().folds.borrow().is_empty() =>
            {
                let range = selection_native_step(&capture, || self.super_selected_range())?;
                if !self.ivars().folds.borrow().is_empty() {
                    return Err(NativeMarkdownFoldError::Stale);
                }
                let end = range
                    .location
                    .checked_add(range.length)
                    .ok_or(NativeMarkdownFoldError::InvalidSelection)?;
                capture.finish(
                    MarkdownTextSelection {
                        anchor_utf16: range.location,
                        head_utf16: end,
                    },
                    false,
                )
            }
            result => result,
        }
    }

    /// 程序选择先由同一纯 Core 发布保存完整方向，再沿该次收据安装有序 Native 范围。
    pub(super) fn set_semantic_native_selection(&self, selection: MarkdownTextSelection) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.ivars().folds.owner_is_current() || self.ivars().textkit_graph_teardown.get() {
            return;
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            return;
        };
        let Ok(Some(storage)) =
            selection_native_step(&capture, || self.ivars().text_storage.load())
        else {
            return;
        };
        let Ok(observed) = self.native_ordered_selection_state_for_capture(&capture) else {
            return;
        };
        let Ok(captured) = capture.finish(selection, true) else {
            return;
        };
        let captured = match observed {
            NativeMarkdownOrderedSelection::Single(before) => {
                match captured.with_observed_native_before(before) {
                    Ok(captured) => captured,
                    Err(_) => return, // 观察与纯发布之间仍核同原票，不能重签一个较新 before。
                }
            }
            NativeMarkdownOrderedSelection::Multiple => captured, // 明确目标可收拢 Multi，但不伪造其原主范围。
        };
        let captured = captured
            .with_head_side(Some(selection.head_utf16 < selection.anchor_utf16))
            .with_vertical_goal(None);
        let Some(committed) = storage.update_active_selection(captured) else {
            return;
        };
        let _ = self.apply_committed_selection(&committed);
    }

    /// 原Core提交票据跨无效化与选择setter保持；不能在Native回调后重新领取新代次。
    pub(super) fn apply_committed_selection(
        &self,
        committed: &NativeMarkdownCommittedSelection,
    ) -> bool {
        self.apply_committed_selection_if(committed, || true)
    }
    pub(super) fn apply_committed_selection_if(
        &self,
        committed: &NativeMarkdownCommittedSelection,
        is_current: impl Fn() -> bool,
    ) -> bool {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !is_current() {
            return false;
        }
        // 字符调用方把实际 pending guard 交到这里；只消费同一 Committed 分配，不能重签任意 EOF。
        let pending = self
            .ivars()
            .pending_selection_slot
            .borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade);
        if let Some(slot) = pending {
            let owner = slot
                .borrow()
                .as_ref()
                .filter(|owner| std::ptr::eq(&*owner.committed, committed))
                .map(Rc::clone);
            if let Some(owner) = owner {
                return self.apply_pending_committed_selection(&owner, &slot, &is_current);
            }
        }
        let Ok(install) = committed.begin_native_install(committed.native_before_range()) else {
            return false; // guard 必须早于第一个 Native getter 或属性/折叠布局调用。
        };
        let Ok(Ok(before)) = committed.native_step(|| self.native_ordered_selection_state()) else {
            return false;
        };
        if matches!(before, NativeMarkdownOrderedSelection::Single(range)
            if install.observe(range) != NativeMarkdownSelectionInstallObservation::OwnDeclared)
            || !is_current()
            || committed
                .native_step(|| self.finish_fold_layout_update())
                .is_err()
            || !is_current()
        {
            return false;
        }
        let selection = committed.selection;
        let start = selection.anchor_utf16.min(selection.head_utf16);
        let end = selection.anchor_utf16.max(selection.head_utf16);
        let affinity = if selection.head_utf16 < selection.anchor_utf16 {
            objc2_app_kit::NSSelectionAffinity::Upstream
        } else {
            objc2_app_kit::NSSelectionAffinity::Downstream
        };
        let installed = committed
            .native_step(|| {
                self.setSelectedRange_affinity_stillSelecting(
                    NSRange::new(start, end - start),
                    affinity,
                    false,
                )
            })
            .is_ok()
            && is_current();
        if !installed {
            return false;
        }
        let Ok(Ok(actual)) = committed.native_step(|| self.native_ordered_selection_range()) else {
            return false;
        };
        actual == NSRange::new(start, end - start)
            && install.observe(actual) == NativeMarkdownSelectionInstallObservation::OwnDeclared
            && committed.current()
            && is_current()
    }

    /// raw 暂态只与原事务声明来源比较，允许删除后的原 before 超过新 EOF；不据它访问正文或补签来源。
    fn pending_native_selection_snapshot(
        &self,
        owner: &Rc<NativeMarkdownPendingSelectionOwner>,
        slot: &NativeMarkdownPendingSelectionSlot,
        is_current: &impl Fn() -> bool,
    ) -> Result<(bool, usize, NSRange), NativeMarkdownFoldError> {
        let _reading = owner.begin_sample().ok_or(NativeMarkdownFoldError::Stale)?;
        pending_selection_native_step(owner, slot, is_current, || {
            autoreleasepool(|_| {
                let ranges: Option<Retained<NSArray<objc2_foundation::NSValue>>> =
                    pending_selection_native_step(owner, slot, is_current, || {
                        // SAFETY: 同一原正文的公开 nullable superclass getter；只读 count，不物化多范围内容。
                        unsafe { msg_send![super(self), selectedRanges] }
                    })?;
                let present = ranges.is_some();
                let count = match &ranges {
                    Some(ranges) => {
                        pending_selection_native_step(owner, slot, is_current, || ranges.count())?
                    }
                    None => 0, // 真实 nil/空数组不是合法 Single，也不是可以猜测的 EOF。
                };
                let raw = pending_selection_native_step(owner, slot, is_current, || {
                    self.super_selected_range()
                })?;
                drop(ranges); // 外层原票门禁覆盖所有 Native claim 和本池析构。
                Ok::<_, NativeMarkdownFoldError>((present, count, raw))
            })
        })?
    }

    /// 字符尾声只消费原事务的有限来源；最终成功仍必须实际 Single exact after 与同一完整 Known。
    fn apply_pending_committed_selection(
        &self,
        owner: &Rc<NativeMarkdownPendingSelectionOwner>,
        slot: &NativeMarkdownPendingSelectionSlot,
        is_current: &impl Fn() -> bool,
    ) -> bool {
        let result = (|| -> Result<(), NativeMarkdownFoldError> {
            let (present, count, before) =
                self.pending_native_selection_snapshot(owner, slot, is_current)?;
            if !owner.observe(present, count, before) {
                return Err(NativeMarkdownFoldError::Stale);
            }
            // 完整逻辑方向只来自原提交收据；Unknown head、已 revoked 或失槽均不能进入 setter。
            let captured = owner.committed.captured_selection()?;
            pending_selection_native_step(owner, slot, is_current, || {
                self.finish_fold_layout_update()
            })?;
            let selection = captured.selection;
            let after = NSRange::new(
                selection.anchor_utf16.min(selection.head_utf16),
                selection.anchor_utf16.abs_diff(selection.head_utf16),
            );
            let affinity = if selection.head_utf16 < selection.anchor_utf16 {
                objc2_app_kit::NSSelectionAffinity::Upstream
            } else {
                objc2_app_kit::NSSelectionAffinity::Downstream
            };
            pending_selection_native_step(owner, slot, is_current, || {
                self.setSelectedRange_affinity_stillSelecting(after, affinity, false)
            })?;
            let (present, count, actual) =
                self.pending_native_selection_snapshot(owner, slot, is_current)?;
            if !present
                || count != 1
                || actual != after
                || self.ivars().folds.known_selection() != Some(selection)
                || !captured.current()
            {
                return Err(NativeMarkdownFoldError::InvalidSelection);
            }
            Ok(())
        })();
        if result.is_err() {
            owner.reject();
        }
        result.is_ok() // 失败不会回退普通两范围安装，也不会 fresh capture 后重试。
    }

    /// 只有仍有完整方向事实的 canonical 选择可覆盖系统 getter；未知范围仍由实际 Native 状态回答。
    pub(super) fn semantic_selection_range(&self) -> Option<NSRange> {
        let capture = self.ivars().folds.capture_selection().ok()?;
        let known = self.ivars().folds.known_selection()?;
        // 这次 finish 仍使用首个 getter 前的原票；完整方向只来自刚读取的已知状态。
        let captured = capture.clone().finish(known, true).ok()?;
        let text_storage =
            selection_native_step(&capture, || self.ivars().text_storage.load()).ok()??;
        let document = text_storage.document()?;
        let range = {
            let document = document.borrow();
            if !captured.current_in(&document) || document.selection() != known {
                return None; // getter 的弱加载重入或 Core 已变更时，不把旧标量返回给新的输入。
            }
            let start = known.anchor_utf16.min(known.head_utf16);
            let end = known.anchor_utf16.max(known.head_utf16);
            let range = NSRange::new(start, end - start);
            is_valid_markdown_replacement_range(range, document.utf16_len()).then_some(range)
        };
        drop(document);
        drop(text_storage);
        // 临时 claims 的析构也可能重入；只复核原票，不为同一个 getter 再捕获较新的选择。
        capture.current().then_some(range).flatten()
    }

    #[cfg(feature = "native-raster-surface-probe")]
    pub(super) fn uses_raster_layer_updates(&self) -> bool {
        self.ivars().raster_update_layer // 构造只读一次环境；不会在帧之间切换入口。
    }

    /// Initialize weak storage/layout backlinks and empty foreground metadata before native construction.
    pub(super) fn new(
        text_storage: &Retained<PieceBackedMarkdownTextStorage>,
        text_container: &NSTextContainer,
        text_layout_manager: &Retained<NSTextLayoutManager>,
        owned_ledger: &Rc<NativeMarkdownOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let document = text_storage
            .document()
            .expect("new editor owns its canonical document");
        let folds = Rc::new(NativeMarkdownFoldState::new(
            &document,
            text_storage.presentation_mode(),
        ));
        *text_storage.ivars().fold_state.borrow_mut() = Rc::downgrade(&folds);
        #[cfg(feature = "native-raster-surface-probe")]
        let raster_update_layer = cfg!(feature = "native-markdown-runtime-probe")
            && super::raster_surface::NativeMarkdownRasterSurface::requested()
            && std::env::var_os("OMNI_NATIVE_RASTER_UPDATE_LAYER")
                .is_some_and(|value| value == "1");
        let text_view = Self::alloc(mtm).set_ivars(PieceBackedTextViewIvars {
            #[cfg(feature = "native-raster-surface-probe")]
            raster_surface: RefCell::new(None),
            #[cfg(feature = "native-raster-surface-probe")]
            raster_update_layer,
            #[cfg(feature = "native-raster-surface-probe")]
            raster_layer_update_calls: Cell::new(0),
            #[cfg(feature = "native-raster-surface-probe")]
            raster_body_layer: RefCell::new(None),
            text_storage: Weak::from_retained(text_storage),
            #[cfg(all(
                feature = "native-block-fragment-diagnostic",
                feature = "native-markdown-runtime-probe"
            ))]
            frozen_values_enabled: text_storage.ivars().string.ivars().frozen_values.is_some(),
            source_decorations: RefCell::new(Vec::new()),
            source_decoration_generation: Cell::new(0),
            source_decorations_dirty: Cell::new(true),
            preview_blocks: RefCell::new(None),
            preview_block_fold_epoch: Cell::new(0),
            preview_block_intent_epoch: Cell::new(0),
            preview_block_attempt: RefCell::new(None),
            gutter: OnceCell::new(),
            folds,
            marked_text_range: Rc::new(Cell::new(None)),
            input_callback: RefCell::new(None),
            pending_selection_slot: RefCell::new(Some(Rc::downgrade(
                &text_storage.ivars().pending_selection,
            ))),
            unmarking_text: Cell::new(false),
            text_layout_manager: OnceCell::new(),
            text_container_recovery_depth: Cell::new(0),
            textkit_graph_teardown: Cell::new(false),
            textkit_input_depth: Cell::new(0),
            payload_use_depth: Cell::new(0),
            visible_table_layout_cache: RefCell::new(None),
            table_layout_update_scheduled: Cell::new(false),
            table_layout_rebuilding: Cell::new(false),
            table_mutation_anchor: RefCell::new(None),
            find_reveal: RefCell::new(None),
            mode_viewport: RefCell::new(None),
            table_input_intent_epoch: Cell::new(0),
            #[cfg(feature = "native-table-row-append")]
            table_append: RefCell::new(None),
            table_field_expectation: RefCell::new(None),
            table_field_notification_checking: Cell::new(false),
            cell_editor: RefCell::new(None),
            owned_ledger: Rc::downgrade(owned_ledger),
            controller: OnceCell::new(),
            active_table_cell: RefCell::new(None),
            pending_table_cell_activation: NativeMarkdownPendingTableCell::default(),
        });
        // SAFETY: all Rust ivars are initialized, and the surface already owns the live container,
        // primary TextKit 2 manager and content store. This is NSTextView's designated initializer;
        // it borrows that exact graph on the main thread instead of constructing a competing network.
        let text_view: Retained<Self> = unsafe {
            msg_send![super(text_view), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)), textContainer: text_container]
        };
        // Preserve only a weak recovery backlink; the enclosing editor retains this exact manager.
        let _ = text_view
            .ivars()
            .text_layout_manager
            .set(Weak::from_retained(text_layout_manager));
        text_view
    }

    /// 表格 delegate 只把真实字段交给同窗口 controller，不重新取得或拥有会话。
    fn observe_table_field_editing(&self, notification: &NSNotification) {
        if self.ivars().textkit_graph_teardown.get() {
            // 封闭 view 不得因迟到字段通知重建历史或观察状态。
            return;
        }
        let Some(field) = notification
            .object()
            .and_then(|object| object.downcast::<NSTextField>().ok())
        else {
            // 非字段通知没有共享编辑器身份。
            return;
        };
        self.observe_active_table_field_selection(None); // 输入通知不猜非空方向，真实 caret 可以精确成为后继跨格 anchor。
        if let Some(controller) = self.ivars().controller.get().and_then(Weak::load) {
            // 只在实际编辑通知内查询实际 currentEditor，借用已结束。
            let borrowed = controller.ivars().borrowed.borrow().clone();
            let _ = borrowed.observe_editing(
                &field,
                &controller.ivars().undo,
                &controller.ivars().owned,
            );
            controller.queue_history_availability_refresh(); // 只因实际字段 begin/change 更新；不在 draw 或布局中查询历史。
        }
    }

    /// 释放所有 Rust 业务容量和任务载荷；只能在可失败预检与所有权接受完成后调用。
    pub(super) fn release_payload(&self) {
        self.ivars().textkit_graph_teardown.set(true);
        #[cfg(feature = "native-raster-surface-probe")]
        {
            let lease = self.ivars().raster_body_layer.borrow_mut().take();
            if let Some(lease) = lease {
                // 原scroll与TextKit图已经断开；借用外仅清本实际layer，并恢复构造前原关系。
                if let Err(error) = lease.restore() {
                    eprintln!(
                        "native_raster_surface event=body_layer_restore_failed error={error:?} formal=false"
                    );
                }
                drop(lease);
            }
        }
        #[cfg(feature = "native-table-row-append")]
        {
            self.cancel_pending_table_append();
        }
        let expectation = { self.ivars().table_field_expectation.borrow_mut().take() };
        self.ivars().table_field_notification_checking.set(false);
        drop(expectation); // 所有表格命令共用具名期待；关闭后Native值在借用外释放。
        self.ivars().folds.seal();
        self.cancel_pending_find_reveal();
        self.cancel_pending_mode_viewport();
        self.release_preview_blocks();
        self.ivars().source_decorations_dirty.set(false);
        self.ivars().table_layout_update_scheduled.set(false);
        self.ivars().table_layout_rebuilding.set(false);
        self.ivars().table_mutation_anchor.borrow_mut().take();
        self.ivars().table_input_intent_epoch.set(0);
        self.ivars().pending_table_cell_activation.set(None);
        self.ivars().marked_text_range.set(None);
        self.ivars().input_callback.borrow_mut().take();
        self.ivars().pending_selection_slot.borrow_mut().take();
        self.ivars().unmarking_text.set(false);
        self.ivars().textkit_input_depth.set(0);
        let folds = self.ivars().folds.replace(Vec::new());
        let decorations = self.ivars().source_decorations.replace(Vec::new());
        let tables = self.ivars().visible_table_layout_cache.borrow_mut().take();
        let cell_state = self.ivars().active_table_cell.borrow_mut().take();
        drop((folds, decorations, tables, cell_state));
    }

    /// 检查具体 view 空壳的业务字段和公开原生关系，不依据运行时类名放行。
    pub(super) fn payload_is_empty(&self) -> bool {
        let pending_empty = self.ivars().pending_selection_slot.borrow().is_none();
        #[cfg(feature = "native-raster-surface-probe")]
        let pending_empty = pending_empty && self.ivars().raster_body_layer.borrow().is_none();
        let metadata_empty = pending_empty
            && {
                #[cfg(feature = "native-table-row-append")]
                {
                    self.ivars().table_append.borrow().is_none()
                }
                #[cfg(not(feature = "native-table-row-append"))]
                {
                    true
                }
            }
            && self.ivars().table_field_expectation.borrow().is_none()
            && !self.ivars().table_field_notification_checking.get()
            && self.ivars().textkit_graph_teardown.get()
            && self.ivars().preview_blocks.borrow().is_none()
            && self.ivars().preview_block_attempt.borrow().is_none()
            && self.ivars().folds.borrow().capacity() == 0
            && self.ivars().source_decorations.borrow().capacity() == 0
            && self.ivars().visible_table_layout_cache.borrow().is_none()
            && self.ivars().active_table_cell.borrow().is_none()
            && self.ivars().cell_editor.borrow().is_none()
            && self.ivars().pending_table_cell_activation.is_empty()
            && !self.ivars().table_layout_update_scheduled.get()
            && !self.ivars().table_layout_rebuilding.get()
            && self.ivars().table_mutation_anchor.borrow().is_none()
            && self.ivars().find_reveal.borrow().is_none()
            && self.ivars().mode_viewport.borrow().is_none()
            && self.ivars().table_input_intent_epoch.get() == 0
            && self.ivars().marked_text_range.get().is_none()
            && self.ivars().input_callback.borrow().is_none()
            && self.ivars().textkit_input_depth.get() == 0
            && self.ivars().payload_use_depth.get() == 0;
        // 原生 getter 前结束所有状态临时借用；检查空壳时同样不能跨可重入调用持 Ref。
        metadata_empty
            && !self.isEditable()
            && !self.isSelectable()
            && !self.allowsUndo()
            && self.delegate().is_none()
            && self.string().length() == 0
            && !self.native_marked_state().0
            && is_empty_native_markdown_selection(self.selectedRange())
            // 直接验证 superclass，避免 closed adapter 的合法无选择返回值掩盖历史原生 caret。
            && is_empty_native_markdown_selection(self.super_selected_range())
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// 只在失败时一次性读取现有每项谓词及容量，不输出正文/路径/地址或省略残留角色。
    pub(super) fn payload_failure_diagnostic(&self) -> String {
        let sealed = self.ivars().textkit_graph_teardown.get();
        let (folds_len, folds_capacity) = {
            let folds = self.ivars().folds.borrow();
            (folds.len(), folds.capacity())
        };
        let (decorations_len, decorations_capacity) = {
            let decorations = self.ivars().source_decorations.borrow();
            (decorations.len(), decorations.capacity())
        };
        let table_cache_none = self.ivars().visible_table_layout_cache.borrow().is_none();
        let active_cell_none = self.ivars().active_table_cell.borrow().is_none();
        let cell_owner_none = self.ivars().cell_editor.borrow().is_none();
        let pending_cell_none = self.ivars().pending_table_cell_activation.is_empty();
        let layout_scheduled = self.ivars().table_layout_update_scheduled.get();
        let layout_rebuilding = self.ivars().table_layout_rebuilding.get();
        let semantic_marked_none = self.ivars().marked_text_range.get().is_none();
        let input_depth = self.ivars().textkit_input_depth.get();
        let payload_depth = self.ivars().payload_use_depth.get();
        // 仅复制标量后再调用公开 native getters；临时对象不逃出此单次失败诊断。
        let editable = self.isEditable();
        let selectable = self.isSelectable();
        let allows_undo = self.allowsUndo();
        let delegate_none = self.delegate().is_none();
        let string_units = self.string().length();
        let (native_marked, native_marked_range) = self.native_marked_state();
        let selection = self.selectedRange();
        let super_selection = self.super_selected_range();
        format!(
            "sealed={sealed},folds_len={folds_len},folds_capacity={folds_capacity},decorations_len={decorations_len},decorations_capacity={decorations_capacity},table_cache_none={table_cache_none},active_cell_none={active_cell_none},cell_owner_none={cell_owner_none},pending_cell_none={pending_cell_none},layout_scheduled={layout_scheduled},layout_rebuilding={layout_rebuilding},semantic_marked_none={semantic_marked_none},input_depth={input_depth},payload_depth={payload_depth},editable={editable},selectable={selectable},allows_undo={allows_undo},delegate_none={delegate_none},string_units={string_units},native_marked={native_marked},native_marked_location={},native_marked_length={},selection_location={},selection_length={},super_selection_location={},super_selection_length={}",
            native_marked_range.location,
            native_marked_range.length,
            selection.location,
            selection.length,
            super_selection.location,
            super_selection.length,
        )
    }

    /// Commit visible input before applying one canonical memento and its receipt selection.
    pub(super) fn restore_history(&self, direction: MarkdownHistoryDirection) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let pending_tail = RefCell::new(None); // 原 history owner 跨安装及所有本地 Native claims 析构。
        if self.ivars().textkit_graph_teardown.get() {
            // accepted release 后迟到入口不再读取外部数据、调用业务或恢复原生编辑。
            return;
        }

        if !self.commit_cell_before_command() {
            // An uncommittable visible cell draft vetoes history navigation instead of disappearing.
            return;
        }
        if !self.commit_composition_before_command() {
            // Native composition failure vetoes structural history without accepting an unsaved later mutation.
            return;
        }
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // A disconnected weak storage cannot mutate or navigate a successor document.
            return;
        };
        // storage通知与selection重整都可能改clip；必须先捕获原有source屏幕位置。
        let mutation_anchor = self.capture_table_mutation_anchor();
        self.table_edit_diagnostic("history_before_storage");
        let (edit, committed, pending) = match text_storage.restore_history(direction) {
            Ok(Some(result)) => result,
            Ok(None) => return, // 空历史不构造变化或移动fold。
            Err(error) => {
                present_native_markdown_error(&error);
                return;
            }
        };
        drop(pending_tail.replace(pending));
        if committed
            .native_step(|| self.table_edit_diagnostic("history_after_storage"))
            .is_err()
        {
            return;
        }
        if committed
            .native_step(|| {
                self.arm_table_mutation_anchor(
                    mutation_anchor,
                    &edit.replaced_utf16_range,
                    edit.change_in_utf16_length,
                )
            })
            .is_err()
        {
            return;
        }
        if !self.apply_committed_selection(&committed) {
            return;
        }
        if committed
            .native_step(|| self.table_edit_diagnostic("history_after_selection"))
            .is_err()
        {
            return;
        }
        if committed
            .native_step(|| self.schedule_visible_table_layout_update())
            .is_err()
        {
            return;
        }
        let _ = committed.native_step(|| self.finish_table_mutation_continuity());
    }

    /// Read the superclass state directly, avoiding a semantic adapter masking a native provisional span.
    fn native_marked_state(&self) -> (bool, NSRange) {
        // SAFETY: the matching public NSTextInputClient getters are synchronous on this live main-thread superclass.
        unsafe {
            (
                msg_send![super(self), hasMarkedText],
                msg_send![super(self), markedRange],
            )
        }
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// Expose the same actual superclass composition observation used by production preflight.
    pub(super) fn native_marked_state_for_probe(&self) -> (bool, NSRange) {
        self.native_marked_state()
    }

    /// Commit both native and semantic composition, rejecting a remaining provisional span before save/teardown.
    pub(super) fn commit_composition_before_release(&self) -> Result<(), String> {
        let input_client = ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super());
        if input_client.hasMarkedText() || self.native_marked_state().0 {
            // The standard input hook commits either visible adapter state or an outstanding native-only span.
            input_client.unmarkText();
        }
        let native = self.native_marked_state();
        if input_client.hasMarkedText()
            || native.0
            || (native.1.location != NSNotFound as usize && native.1.length != 0)
        {
            // A native refusal cannot be hidden by clearing only custom metadata or discarding its source owner.
            return Err("笔记输入尚未完成，无法提交".into());
        }
        if let Some(text_storage) = self.ivars().text_storage.load() {
            // Flush finite derived invalidation only after both native and canonical composition have ended.
            text_storage.finish_composition();
        }
        Ok(())
    }

    /// Convert native composition rejection into a consumed command while keeping its editable state visible.
    pub(super) fn commit_composition_before_command(&self) -> bool {
        match self.commit_composition_before_release() {
            // No provisional text remains; subsequent persistence/history may proceed.
            Ok(()) => true,
            // Native void/bool commands report failure and must not fall through into another edit.
            Err(error) => {
                present_native_markdown_error(&error);
                false
            }
        }
    }
}

/// 空文档只能保留起始空 caret 或协议规定的无选择；任意历史非零偏移都失败。
fn is_empty_native_markdown_selection(range: NSRange) -> bool {
    range.length == 0 && (range.location == 0 || range.location == NSNotFound as usize)
}

/// Rejects NSNotFound, overflow and out-of-document native replacement geometry.
pub(super) fn is_valid_markdown_replacement_range(range: NSRange, document_length: usize) -> bool {
    range.location != NSNotFound as usize
        && range
            .location
            .checked_add(range.length)
            .is_some_and(|end| end <= document_length)
}

/// 只沿本次新增候选检查 UTF16 scalar 边界，不按字节或 surrogate 半段接受相对选择。
fn input_text_has_utf16_boundary(text: &str, offset: usize) -> bool {
    let mut current = 0;
    for scalar in text.chars() {
        if current == offset {
            return true; // 恰好位于完整 scalar 前，允许空选择或这一端点。
        }
        current += scalar.len_utf16();
        if current > offset {
            return false; // 落在 supplementary scalar 的低 surrogate 前不是合法 Core 端点。
        }
    }
    current == offset // 末端与空候选的 0 都是合法边界，超出候选则拒绝。
}

#[cfg(test)]
mod closed_selection_tests {
    use super::*;

    #[test]
    fn empty_native_selection_rejects_historic_caret_and_nonempty_ranges() {
        // 两个允许值分别是空正文起点和 NSTextInputClient 明确的 no-selection sentinel。
        assert!(is_empty_native_markdown_selection(NSRange::new(0, 0)));
        assert!(is_empty_native_markdown_selection(NSRange::new(
            NSNotFound as usize,
            0
        )));
        for range in [
            NSRange::new(10_622, 0),
            NSRange::new(1, 0),
            NSRange::new(usize::MAX, 0),
            NSRange::new(0, 1),
            NSRange::new(NSNotFound as usize, 1),
        ] {
            // 不允许以 length=0 掩盖越过空 string 的旧位置，也不允许 sentinel 携带内容范围。
            assert!(!is_empty_native_markdown_selection(range));
        }
    }
}

/// TK2尚无状态时的有限判定；实际位置还必须由当前文档验证UTF16标量边界。
fn single_native_caret(count: usize, range: NSRange) -> Option<MarkdownTextSelection> {
    // 多caret的selectedRange只能给其中之一；非空范围缺少真实方向也不能伪装成main。
    (count == 1
        && range.length == 0
        && range.location != NSNotFound as usize
        && range.location != usize::MAX)
        .then(|| MarkdownTextSelection::caret(range.location))
}

#[cfg(test)]
mod fold_selection_tests {
    use super::*;

    #[test]
    fn missing_tk2_selection_accepts_only_a_proven_single_native_caret() {
        assert_eq!(
            single_native_caret(1, NSRange::new(0, 0)),
            Some(MarkdownTextSelection::caret(0))
        );
        assert_eq!(
            single_native_caret(1, NSRange::new(7, 0)),
            Some(MarkdownTextSelection::caret(7))
        );
        for (count, range) in [
            (0, NSRange::new(0, 0)),
            (2, NSRange::new(7, 0)),
            (1, NSRange::new(0, 3)),
            (1, NSRange::new(NSNotFound as usize, 0)),
            (1, NSRange::new(usize::MAX, 0)),
        ] {
            assert!(single_native_caret(count, range).is_none());
        }
    }
}

/// 临时Native返回值不进入state借用；权限检查本身只读Rust Weak/代次/TLS。
pub(super) fn selection_native_step<T>(
    capture: &NativeMarkdownSelectionCapture,
    work: impl FnOnce() -> T,
) -> Result<T, NativeMarkdownFoldError> {
    if !capture.current() {
        return Err(NativeMarkdownFoldError::Stale);
    }
    let result = work();
    if !capture.current() {
        return Err(NativeMarkdownFoldError::Stale);
    }
    Ok(result)
}
