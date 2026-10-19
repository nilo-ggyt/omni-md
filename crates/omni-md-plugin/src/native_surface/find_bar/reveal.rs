//! 单个Find目标贯穿既有有界布局事务；只有真实目标首个字形进入clip才发布成功。

use super::*;

/// 显式原生诊断只记录公开单调时钟与固定阶段名，不取得正文、对象或新的动作权限。
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_find_native_phase(phase: &'static str) {
    native_markdown_trace! {
        // SAFETY: 签名对应系统mach_time.h的无参uint64时钟；此调用不访问AppKit或产生回调。
        unsafe extern "C" {
            fn mach_absolute_time() -> u64;
        }
        // SAFETY: 公共时钟无前置条件；原始ticks由独立采样器的timebase解释，不猜毫秒。
        let ticks = unsafe { mach_absolute_time() };
        eprintln!("[probe] find native phase={phase} awake_ticks={ticks}");
        // 同步采本PID原有账本，区分选区安装与视口迁移；该诊断不签发整产品预算通过。
        crate::native_surface::probe_call_diagnostic::memory_point(phase);
    }
}

/// 有限诊断只使用本次动作已读取的标量，不为观察补做布局或原生查询。
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_find_geometry(
    phase: &'static str,
    probe: &std::ops::Range<usize>,
    clip: NSRect,
    frame: Option<NSRect>,
    target: NSPoint,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if COUNT.fetch_add(1, Ordering::Relaxed) >= 64 {
        return; // 诊断容量不随文档长度或后续Find动作增长。
    }
    eprintln!(
        "native_find_geometry phase={phase} probe={probe:?} clip={clip:?} frame={frame:?} target={target:?}"
    );
}
use objc2_app_kit::{NSTextLayoutManagerSegmentOptions, NSTextLayoutManagerSegmentType};

/// 反馈只说明本次已提交动作；正文或查找字符串绝不进入延后票据。
#[derive(Clone, Copy)]
pub(in crate::native_surface) enum NativeMarkdownFindRevealFeedback {
    Found,
    ReplacedNext,
    ReplacedLast,
}

/// 隐藏导航是入口固定的有限权限，不把隐藏状态泛化为可执行替换/全选的权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native_surface) enum NativeMarkdownFindVisibility {
    Visible,
    HiddenNavigation,
}

impl NativeMarkdownFindVisibility {
    /// 唯有Next/Previous允许沿hide保留的查询继续导航；closed始终拒绝。
    pub(in crate::native_surface) fn capture(
        action: MarkdownFindAction,
        visible: bool,
        closed: bool,
    ) -> Option<Self> {
        if closed {
            return None;
        }
        if visible {
            return Some(Self::Visible);
        }
        matches!(
            action,
            MarkdownFindAction::Next | MarkdownFindAction::Previous
        )
        .then_some(Self::HiddenNavigation)
    }

    /// 显示状态变化撤销旧栈；visible请求不能在hide后自动继承隐藏导航权限。
    pub(in crate::native_surface) fn permits(
        self,
        visible: bool,
        closed: bool,
        expected_revision: u64,
        current_revision: u64,
    ) -> bool {
        // hide/show两次后布尔值可相同，既有单调代次仍须严格等于入口原票据。
        !closed && visible == (self == Self::Visible) && expected_revision == current_revision
    }
}

/// 同一次定位的所有标量权限。相同generation不代表同文档，弱身份另行独立核对。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FindRevealStamp {
    owner: usize,
    generation: u64,
    length: usize,
    width: i64,
    mode: MarkdownPresentationMode,
    input_epoch: u64,
    table_epoch: u64,
}

/// slot、owner关闭态和分配身份都必须成立，不能用Rc仍存活或某一个代次代替授权。
fn find_reveal_permissions(
    expected: FindRevealStamp,
    actual: FindRevealStamp,
    slot: bool,
    foreground: bool,
    document: bool,
    bar_authorized: bool,
) -> bool {
    expected == actual && slot && foreground && document && bar_authorized
}

/// 匹配扫描/替换收据给出的原始身份与坐标；arm不得重新领取当前文档代次。
pub(in crate::native_surface) enum NativeMarkdownFindRevealSelection {
    Captured {
        capture: NativeMarkdownSelectionCapture,
        native_before: Option<NSRange>,
    },
    Committed(Rc<NativeMarkdownCommittedSelection>),
}

pub(in crate::native_surface) struct NativeMarkdownFindRevealTarget {
    pub(in crate::native_surface) document: WeakMarkdownDocument,
    pub(in crate::native_surface) generation: u64,
    pub(in crate::native_surface) range: std::ops::Range<usize>,
    pub(in crate::native_surface) feedback: NativeMarkdownFindRevealFeedback,
    pub(in crate::native_surface) visibility: NativeMarkdownFindVisibility,
    /// 来自匹配动作入口，不在arm时重新读取较新show/hide权限。
    pub(in crate::native_surface) bar_revision: u64,
    pub(in crate::native_surface) table_epoch: u64,
    pub(in crate::native_surface) selection: NativeMarkdownFindRevealSelection,
}

/// 每view唯一有限目标。两个Rust Weak不延长正文/栏的存活，不持Native对象或片段。
pub(in crate::native_surface) struct NativeMarkdownFindRevealRequest {
    document: WeakMarkdownDocument,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    stamp: FindRevealStamp,
    probe: std::ops::Range<usize>,
    feedback: NativeMarkdownFindRevealFeedback,
    visibility: NativeMarkdownFindVisibility,
    bar_revision: u64,
    /// 新Find只沿入口原capture发布一次；替换直接复用已提交收据，不二次推进serial。
    selection_capture: Option<NativeMarkdownSelectionCapture>,
    committed: RefCell<Option<Rc<NativeMarkdownCommittedSelection>>>,
    /// selectedRange回调可重入executor；粗定位结束前不能消费尚未完成的目标。
    ready: Cell<bool>,
    /// 完整有界批次通过后才能消费成功，literal fallback不是完成收据。
    prepared: Cell<bool>,
    /// 独立工作负载只观察原请求终点；产品默认构建不增加收据状态。
    #[cfg(feature = "native-markdown-runtime-probe")]
    completed: Cell<Option<bool>>,
    /// 仅验收构建持一次标量回调；没有正文、窗口或栏的强引用。
    #[cfg(feature = "native-markdown-runtime-probe")]
    terminal_observer: crate::native_surface::probe::ProbeTerminalObserver,
}

#[cfg(feature = "native-markdown-runtime-probe")]
impl NativeMarkdownFindRevealRequest {
    /// 注册前已经完成也只复用原终点；返回 true 不代表成功，后继仍核原 completed 值。
    pub(in crate::native_surface) fn observe_probe_terminal(
        &self,
        observer: impl FnOnce() + 'static,
    ) -> Result<bool, String> {
        self.terminal_observer
            .observe(self.completed.get(), observer)
    }

    /// 只读原请求的实际终点，slot消失和后继栏文案都不能代替该收据。
    pub(in crate::native_surface) fn probe_completed(&self) -> Option<bool> {
        self.completed.get()
    }

    /// 未完成请求被取消时保留失败；已经签发的成功不被正常移除覆写。
    fn record_probe_removal(&self) {
        if self.completed.get().is_none() {
            self.completed.set(Some(false));
        }
        self.terminal_observer.notify(); // 原取消或既有成功均是终点；不在 find_reveal 槽借用内回调。
    }
}

/// 仅同步executor栈内持有实际controller/storage；外层PayloadUseGuard阻止栈内关闭。
pub(in crate::native_surface) struct NativeMarkdownFindRevealContext {
    request: Rc<NativeMarkdownFindRevealRequest>,
    controller: Retained<NativeMarkdownController>,
    storage: Retained<PieceBackedMarkdownTextStorage>,
}

impl NativeMarkdownFindRevealContext {
    /// 所有条件均为Rust元数据，不在原生调用之后用另一个getter重新领取权限。
    pub(in crate::native_surface) fn is_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        if !self.permissions_are_current(view) {
            return false;
        }
        self.request.selection_is_current()
    }

    /// 纯发布callback只沿既有owner/source/bar/input票接收自己的转换，不读取新权限。
    fn permissions_are_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        let request = &self.request;
        let slot_current = view
            .ivars()
            .find_reveal
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, request));
        let identity = self.storage.ivars().document.borrow().clone();
        let Some(document) = identity.upgrade() else {
            return false;
        };
        let actual = {
            let document = document.borrow();
            FindRevealStamp {
                owner: std::ptr::from_ref(&*self.controller).addr(),
                generation: document.generation(),
                length: document.utf16_len(),
                width: self.storage.ivars().table_projection_width_key.get(),
                mode: self.storage.presentation_mode(),
                input_epoch: self.controller.find_input_intent(),
                table_epoch: view.ivars().table_input_intent_epoch.get(),
            }
        };
        find_reveal_permissions(
            request.stamp,
            actual,
            slot_current,
            !view.ivars().textkit_graph_teardown.get()
                && self.storage.ivars().accepts_edits.get()
                && native_markdown_controller_has_foreground_authority(&self.controller),
            std::rc::Weak::ptr_eq(&identity, &request.document),
            request.bar.upgrade().is_some_and(|bar| {
                request.visibility.permits(
                    bar.visible.get(),
                    bar.closed.get(),
                    request.bar_revision,
                    bar.layout_revision.get(),
                )
            }),
        )
    }

    /// cache最终发布后仅标记当前票据；旧局部值不能为新目标签发准备成功。
    pub(in crate::native_surface) fn mark_prepared(&self, view: &PieceBackedMarkdownTextView) {
        if self.is_current(view) {
            self.request.prepared.set(true);
        }
    }

    /// 只布局首个完整Unicode标量；长跨行匹配也不要求全选区同时驻留或全篇布局。
    pub(in crate::native_surface) fn probe(&self) -> std::ops::Range<usize> {
        self.request.probe.clone()
    }
}

impl NativeMarkdownFindRevealRequest {
    fn selection_is_current(&self) -> bool {
        let committed = self.committed.borrow();
        if let Some(committed) = committed.as_ref() {
            // 纯发布callback已接入同Rc后，旧capture撤权是预期转换，只消费原提交后果。
            committed.current()
        } else {
            self.selection_capture
                .as_ref()
                .is_some_and(NativeMarkdownSelectionCapture::current)
        }
    }
}

/// EOF零长匹配保持caret；补充平面首字形不能把surrogate一分为二。
fn first_match_scalar(
    start: usize,
    length: usize,
    first: Option<u16>,
) -> Option<std::ops::Range<usize>> {
    if start > length {
        // 过期或越界坐标不截断成另一个合法位置。
        return None;
    }
    if start == length {
        return Some(start..start);
    }
    let units = if (0xd800..=0xdbff).contains(&first?) {
        2
    } else {
        1
    };
    let end = start.checked_add(units)?;
    (end <= length).then_some(start..end)
}

/// 真实segment须有限且有高度；超宽字形以其起点可见为准，不伪造完整匹配全可见。
fn find_segment_is_visible(frame: NSRect, clip: NSRect) -> bool {
    let Some(origin) = native_markdown_minimal_cell_scroll_origin(frame, clip) else {
        return false; // NaN/无限/零clip没有可见性收据。
    };
    (origin.x - clip.origin.x).abs() <= 0.5 && (origin.y - clip.origin.y).abs() <= 0.5
}

impl PieceBackedMarkdownTextView {
    /// 字段变化只回收先前输入的目标；不能取消其后新建的模式定位或显式Find目标。
    pub(in crate::native_surface) fn cancel_find_reveal_before_input(&self, intent: u64) {
        let previous = self
            .ivars()
            .find_reveal
            .borrow()
            .as_ref()
            .filter(|request| request.stamp.input_epoch < intent)
            .map(Rc::clone);
        if let Some(previous) = previous {
            self.remove_find_reveal_if_same(&previous);
        } // 原slot若已有后继目标，身份核验不会取走它；没有原目标也不动模式槽。
    }

    /// 新命令、resize、物理输入和accepted release同步撤权；drop不跨slot借用。
    pub(in crate::native_surface) fn cancel_pending_find_reveal(&self) {
        self.cancel_pending_mode_viewport(); // 新Find动作不能让上一模式目标在同一视图上迟到恢复。
        let old = self.ivars().find_reveal.borrow_mut().take();
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(old) = old.as_ref() {
            old.record_probe_removal(); // 只写原Rust票据，借用已经结束，不增加Native操作。
        }
        drop(old);
    }

    /// TextKit因真实内容高度更新setFrameSize不算用户改宽；只撤销不同列宽的目标。
    pub(in crate::native_surface) fn cancel_find_reveal_for_width(&self, width: f64) {
        let metric = native_markdown_layout_metric(width);
        let changed = self
            .ivars()
            .find_reveal
            .borrow()
            .as_ref()
            .is_some_and(|request| request.stamp.width != metric);
        if changed {
            self.cancel_pending_find_reveal();
        }
    }

    /// executor只克隆Rust Rc；Native weak.load及retain都在借用之外，且前后核原票据。
    pub(in crate::native_surface) fn pending_find_reveal_context(
        &self,
    ) -> Option<NativeMarkdownFindRevealContext> {
        let request = self.ivars().find_reveal.borrow().as_ref().map(Rc::clone)?;
        if !request.ready.get() || self.ivars().textkit_graph_teardown.get() {
            return None; // 粗定位尚在原生栈内时不抢先消费；关闭由release清slot。
        }
        let controller = self.ivars().controller.get()?.load()?;
        if std::ptr::from_ref(&*controller).addr() != request.stamp.owner
            || controller.find_input_intent() != request.stamp.input_epoch
            || !native_markdown_controller_has_foreground_authority(&controller)
        {
            self.remove_find_reveal_if_same(&request);
            return None;
        }
        let storage = self.ivars().text_storage.load()?;
        let context = NativeMarkdownFindRevealContext {
            request,
            controller,
            storage,
        };
        if context.is_current(self) {
            Some(context)
        } else {
            // Native weak升级中新的输入只撤旧目标，绝不清后继目标。
            self.remove_find_reveal_if_same(&context.request);
            None
        }
    }

    fn remove_find_reveal_if_same(&self, request: &Rc<NativeMarkdownFindRevealRequest>) {
        let old = {
            let mut slot = self.ivars().find_reveal.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, request))
            {
                slot.take()
            } else {
                None // 新目标已经发布，旧executor不能取走它。
            }
        };
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(old) = old.as_ref() {
            old.record_probe_removal();
        }
        drop(old);
    }

    /// 同步Find动作返回后捕获原票；未取得票据就失败，不用较新UI猜测先前成功。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(in crate::native_surface) fn observe_find_reveal_for_probe(
        &self,
    ) -> Option<Rc<NativeMarkdownFindRevealRequest>> {
        self.ivars().find_reveal.borrow().as_ref().map(Rc::clone)
    }

    /// 仅保留既有的一次scrollRangeToVisible作粗定位；后续不是第二次命令/计时重试。
    pub(in crate::native_surface) fn begin_find_reveal(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        intent: u64,
        target: NativeMarkdownFindRevealTarget,
    ) -> Result<(), String> {
        let current = || {
            native_markdown_controller_has_foreground_authority(controller)
                && controller.find_input_intent() == intent
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == target.table_epoch
                && target.visibility.permits(
                    bar.visible.get(),
                    bar.closed.get(),
                    target.bar_revision,
                    bar.layout_revision.get(),
                )
        };
        if !current() {
            return Err("查找定位已被新的输入或关闭撤销".into());
        }
        let selection = MarkdownTextSelection {
            anchor_utf16: target.range.start,
            head_utf16: target.range.end,
        };
        let (original_capture, captured, committed) = match target.selection {
            NativeMarkdownFindRevealSelection::Captured {
                capture,
                native_before,
            } => {
                // 完整方向由明确Find目标给出；原capture在查询getter之前固定，不从当前选区重领。
                let captured = capture
                    .clone()
                    .finish(selection, true)
                    .map_err(|error| error.to_string())?;
                let captured = if let Some(native_before) = native_before {
                    captured
                        .with_observed_native_before(native_before)
                        .map_err(|error| error.to_string())?
                } else {
                    // 实际多选没有单before或主head；明确Find目标仍沿原capture，不伪造旧单范围。
                    captured
                }
                .with_head_side(Some(false))
                .with_vertical_goal(None);
                (Some(capture), Some(captured), None)
            }
            NativeMarkdownFindRevealSelection::Committed(committed) => {
                if !committed.current() || committed.selection != selection {
                    // 替换自己的字符收据必须精确对应这个目标，不得再次发布同一选择以补新serial。
                    return Err("替换后的查找选择收据已失效".into());
                }
                (None, None, Some(committed))
            }
        };
        let storage = self.ivars().text_storage.load().ok_or("笔记编辑器已关闭")?;
        if !current()
            || original_capture
                .as_ref()
                .is_some_and(|capture| !capture.current())
            || committed
                .as_ref()
                .is_some_and(|committed| !committed.current())
        {
            return Err("查找定位已失效".into());
        }
        let identity = storage.ivars().document.borrow().clone();
        if !std::rc::Weak::ptr_eq(&identity, &target.document) {
            return Err("查找所属笔记已变化".into());
        }
        let document = identity.upgrade().ok_or("笔记文档已关闭")?;
        let belongs = captured
            .as_ref()
            .map(|captured| captured.belongs_to(&document))
            .or_else(|| {
                committed.as_ref().map(|committed| {
                    committed
                        .captured_selection()
                        .is_ok_and(|captured| captured.belongs_to(&document))
                })
            })
            .unwrap_or(false);
        if !belongs {
            return Err("查找选择不属于原笔记收据".into());
        }
        let (generation, length, probe) = {
            let document = document.borrow();
            if document.generation() != target.generation {
                return Err("查找定位期间正文已变更".into());
            }
            let length = document.utf16_len();
            let first = (target.range.start < length)
                .then(|| document.utf16_code_unit_at(target.range.start).ok())
                .flatten();
            (
                document.generation(),
                length,
                first_match_scalar(target.range.start, length, first).ok_or("查找定位范围无效")?,
            )
        };
        if target.range.end > length || target.range.end < target.range.start || !current() {
            return Err("查找定位范围已失效".into());
        }
        let request = Rc::new(NativeMarkdownFindRevealRequest {
            document: identity,
            bar: Rc::downgrade(bar),
            stamp: FindRevealStamp {
                owner: std::ptr::from_ref(controller).addr(),
                generation,
                length,
                width: storage.ivars().table_projection_width_key.get(),
                mode: storage.presentation_mode(),
                input_epoch: intent,
                table_epoch: target.table_epoch,
            },
            probe,
            feedback: target.feedback,
            visibility: target.visibility,
            bar_revision: target.bar_revision,
            selection_capture: original_capture,
            committed: RefCell::new(committed),
            ready: Cell::new(false),
            prepared: Cell::new(false),
            #[cfg(feature = "native-markdown-runtime-probe")]
            completed: Cell::new(None),
            #[cfg(feature = "native-markdown-runtime-probe")]
            terminal_observer: crate::native_surface::probe::ProbeTerminalObserver::default(),
        });
        // 先取得局部原生owner；失败不能留下ready=false而永远阻止cache命中的票据。
        if !current() || !request.selection_is_current() {
            return Err("查找输入已变更".into());
        }
        let actual_controller = self
            .ivars()
            .controller
            .get()
            .and_then(Weak::load)
            .ok_or("查找所属窗口已释放")?;
        if !current() || !request.selection_is_current() {
            return Err("查找输入已变更".into());
        }
        if std::ptr::from_ref(&*actual_controller).addr() != std::ptr::from_ref(controller).addr() {
            return Err("查找栏已由其他操作接管".into());
        }
        let previous = self.ivars().find_reveal.replace(Some(Rc::clone(&request)));
        #[cfg(feature = "native-markdown-runtime-probe")]
        if let Some(previous) = previous.as_ref() {
            previous.record_probe_removal(); // 后继目标不能让被替换的旧收据永久停留在未完成。
        }
        drop(previous);
        // 局部context不跨事件；仅用于对每个Native步骤前后认证同一个目标。
        let context = NativeMarkdownFindRevealContext {
            request,
            controller: actual_controller,
            storage,
        };
        let is_current = || context.is_current(self);
        let result = (|| {
            native_markdown_table_intent_step(&is_current, || {
                if bar.is_visible() {
                    // 隐藏Cmd-G只导航，不挂载栏、不改变输入焦点或为反馈布局隐藏控件。
                    bar.status.setStringValue(ns_string!("正在定位匹配…"));
                }
            })?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_native_phase("selection_begin");
            let committed = if let Some(captured) = captured {
                if !is_current() {
                    return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
                }
                context
                    .storage
                    .update_active_selection_observed(captured, |committed| {
                        // 无Native、无Doc借用；只把原转换收据接到同Rc请求，再允许attrs重入。
                        if context.permissions_are_current(self)
                            && context.request.committed.borrow().is_none()
                        {
                            *context.request.committed.borrow_mut() = Some(Rc::clone(committed));
                        }
                    })
                    .ok_or(NativeMarkdownStableTableFailure::InputIntentExpired)?
            } else {
                context
                    .request
                    .committed
                    .borrow()
                    .as_ref()
                    .map(Rc::clone)
                    .ok_or(NativeMarkdownStableTableFailure::InputIntentExpired)?
            };
            let same_receipt = context
                .request
                .committed
                .borrow()
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &committed));
            if !same_receipt || !self.apply_committed_selection_if(&committed, is_current) {
                return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_native_phase("selection_return");
            // 完整选择不变；由原生 viewport 直接迁移至原首 scalar，避免通用滚动入口向前补排离屏内容。
            self.relocate_find_viewport(&context, &is_current)?;
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_native_phase("coarse_scroll_return");
            Ok::<(), NativeMarkdownStableTableFailure>(())
        })();
        if result.is_err() {
            // getter/setter重入撤权时只移除旧票据，不触及已发布的后继slot。
            self.remove_find_reveal_if_same(&context.request);
            return Err("查找定位已失效".into());
        }
        context.request.ready.set(true);
        self.schedule_visible_table_layout_update();
        Ok(())
    }

    /// 返回第一个实际字形矩形；block只拷贝一个标量rect且立即停止，不保留原生range。
    fn find_reveal_segment(
        &self,
        context: &NativeMarkdownFindRevealContext,
    ) -> Result<NSRect, NativeMarkdownStableTableFailure> {
        self.source_probe_segment(&context.probe(), &|| context.is_current(self))
    }

    /// 粗定位只迁移原请求的 viewport；建议锚点不是查找成功，后续仍须验证实际首字形与 clip。
    fn relocate_find_viewport(
        &self,
        context: &NativeMarkdownFindRevealContext,
        current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let manager = native_markdown_table_intent_step(current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let content = native_markdown_table_intent_step(current, || manager.textContentManager())?
            .ok_or(Failure::MissingGeometry)?;
        let require_original_content_graph = || -> Result<(), Failure> {
            let actual_manager =
                native_markdown_table_intent_step(current, || self.textLayoutManager())?
                    .ok_or(Failure::MissingGeometry)?;
            let actual_content =
                native_markdown_table_intent_step(current, || manager.textContentManager())?
                    .ok_or(Failure::MissingGeometry)?;
            if !std::ptr::eq(&*actual_manager, &*manager)
                || !std::ptr::eq(&*actual_content, &*content)
            {
                return Err(Failure::InvalidSourceAssociation); // 同代次状态不能授权已脱离视图的原生图。
            }
            Ok(())
        };
        require_original_content_graph()?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document = native_markdown_table_intent_step(current, || provider.documentRange())?;
        let start = native_markdown_table_intent_step(current, || document.location())?;
        let offset = isize::try_from(context.request.probe.start)
            .map_err(|_| Failure::InvalidSourceAssociation)?;
        let location = native_markdown_table_intent_step(current, || {
            content.locationFromLocation_withOffset(&start, offset)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        if native_markdown_table_intent_step(current, || {
            content.offsetFromLocation_toLocation(&start, &location)
        })? != offset
        {
            // 原生位置必须往返同一 canonical 偏移；不能把估计位置当作原查找匹配。
            return Err(Failure::InvalidSourceAssociation);
        }
        let visible = native_markdown_table_intent_step(current, || self.visibleRect())?;
        let visible_source = self.read_visible_utf16_window(visible, context.request.stamp.length);
        if !current() {
            return Err(Failure::InputIntentExpired); // 行读取可重入；旧查找不能继续迁移后继视口。
        }
        require_original_content_graph()?;
        if !visible_source.is_empty()
            && visible_source.start <= context.request.probe.start
            && context.request.probe.end <= visible_source.end
        {
            // 已在真实可见行中的目标保留当前视口；不因粗迁移重新估算已排好的段落位置。
            // 后续有限ensure和实际字形矩形仍须通过，源行相交本身不签发Find成功。
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_find_geometry(
                "already_in_viewport",
                &context.probe(),
                visible,
                None,
                visible.origin,
            );
            return Ok(());
        }
        let viewport =
            native_markdown_table_intent_step(current, || manager.textViewportLayoutController())?;
        let actual_manager =
            native_markdown_table_intent_step(current, || viewport.textLayoutManager())?
                .ok_or(Failure::MissingGeometry)?;
        if !std::ptr::eq(&*actual_manager, &*manager) {
            // getter 重入导致关联变化时停止，不迁移后继 manager 的 viewport。
            return Err(Failure::InvalidSourceAssociation);
        }
        let require_original_graph = || -> Result<(), Failure> {
            require_original_content_graph()?;
            let actual_viewport_manager =
                native_markdown_table_intent_step(current, || viewport.textLayoutManager())?
                    .ok_or(Failure::MissingGeometry)?;
            if !std::ptr::eq(&*actual_viewport_manager, &*manager) {
                // relocate 或后续 getter 可重入；旧 manager 的建议锚点不能应用到后继原生图。
                return Err(Failure::InvalidSourceAssociation);
            }
            Ok(())
        };
        require_original_graph()?; // 先证明viewport仍属于本视图，不能迁移之后才发现旧图已被替换。
        let anchor = native_markdown_table_intent_step(current, || {
            viewport.relocateViewportToTextLocation(&location)
        })?;
        require_original_graph()?;
        let origin = native_markdown_table_intent_step(current, || self.textContainerOrigin())?;
        let visible = native_markdown_table_intent_step(current, || self.visibleRect())?;
        let target_y = anchor + origin.y;
        if !target_y.is_finite() || !visible.origin.x.is_finite() {
            // 建议位置无效时明确失败；不改用另一个滚动 API 重试或猜测目标页。
            return Err(Failure::MissingGeometry);
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_geometry(
            "relocate",
            &context.probe(),
            visible,
            None,
            NSPoint::new(visible.origin.x, target_y),
        );
        let scroll = native_markdown_table_intent_step(current, || self.enclosingScrollView())?
            .ok_or(Failure::MissingGeometry)?;
        let clip = native_markdown_table_intent_step(current, || scroll.contentView())?;
        let document_view = native_markdown_table_intent_step(current, || clip.documentView())?
            .ok_or(Failure::MissingGeometry)?;
        let original_view: &NSView = self;
        if !std::ptr::eq(&*document_view, original_view) {
            // 原 scroll 仍存活不代表当前仍挂着原文本视图，拒绝移动其他 document view。
            return Err(Failure::InvalidSourceAssociation);
        }
        require_original_graph()?;
        native_markdown_table_intent_step(current, || {
            // 保留横向 clip；EOF 和文首由现有 NSClipView 约束，随后真实 segment 会核对实际结果。
            clip.scrollToPoint(NSPoint::new(visible.origin.x, target_y.max(0.0)));
        })?;
        native_markdown_table_intent_step(current, || scroll.reflectScrolledClipView(&clip))?;
        Ok(())
    }

    /// 查找和模式连续性共用首scalar/EOF的真实几何，不共享两者各自的授权票据。
    pub(in crate::native_surface) fn source_probe_segment(
        &self,
        probe: &std::ops::Range<usize>,
        current: &impl Fn() -> bool,
    ) -> Result<NSRect, NativeMarkdownStableTableFailure> {
        self.source_probe_segment_with_affinity(probe, false, current)
    }

    /// 空 caret 在换行边界可以处于两条视觉行；选择导航必须保留已证明的那一侧。
    pub(in crate::native_surface) fn source_probe_segment_with_affinity(
        &self,
        probe: &std::ops::Range<usize>,
        upstream: bool,
        current: &impl Fn() -> bool,
    ) -> Result<NSRect, NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let manager = native_markdown_table_intent_step(&current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let content = native_markdown_table_intent_step(&current, || manager.textContentManager())?
            .ok_or(Failure::MissingGeometry)?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let native_document =
            native_markdown_table_intent_step(&current, || provider.documentRange())?;
        let start = native_markdown_table_intent_step(&current, || native_document.location())?;
        let from = isize::try_from(probe.start).map_err(|_| Failure::InvalidSourceAssociation)?;
        let to = isize::try_from(probe.end).map_err(|_| Failure::InvalidSourceAssociation)?;
        let location = native_markdown_table_intent_step(&current, || {
            content.locationFromLocation_withOffset(&start, from)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let end = native_markdown_table_intent_step(&current, || {
            content.locationFromLocation_withOffset(&start, to)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let allocated = native_markdown_table_intent_step(&current, NSTextRange::alloc)?;
        let range = native_markdown_table_intent_step(&current, || {
            NSTextRange::initWithLocation_endLocation(allocated, &location, Some(&end))
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let origin = native_markdown_table_intent_step(&current, || self.textContainerOrigin())?;
        let frame = Cell::new(None);
        let block = block2::RcBlock::new(
            |_range: *mut NSTextRange,
             rect: NSRect,
             _baseline: f64,
             _container: NonNull<NSTextContainer>| {
                // 回调内不做getter；只接受首个真实有限矩形，不扩张成全选区/全段layout。
                frame.set(Some(NSRect::new(
                    NSPoint::new(rect.origin.x + origin.x, rect.origin.y + origin.y),
                    NSSize::new(rect.size.width.max(1.0), rect.size.height),
                )));
                objc2::runtime::Bool::NO
            },
        );
        let mut options = NSTextLayoutManagerSegmentOptions::RangeNotRequired;
        if probe.is_empty() && upstream {
            // 非空探针仍使用既有字形矩形；只有明确空位置才使用换行上游语义。
            options |= NSTextLayoutManagerSegmentOptions::UpstreamAffinity;
        }
        native_markdown_table_intent_step(&current, || {
            manager.enumerateTextSegmentsInRange_type_options_usingBlock(
                &range,
                NSTextLayoutManagerSegmentType::Selection,
                options,
                &block,
            )
        })?;
        drop(block);
        frame
            .get()
            .filter(|rect| rect.size.height.is_finite() && rect.size.height > 0.0)
            .ok_or(Failure::MissingGeometry)
    }

    /// 无表格/Source分支也沿同一目标完成有限ensure与真实viewport布局，不绕过认证。
    pub(in crate::native_surface) fn prepare_find_reveal_without_tables(
        &self,
        context: &NativeMarkdownFindRevealContext,
        current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        self.ensure_stable_table_source_range(&context.probe(), current)?;
        let manager = native_markdown_table_intent_step(current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let viewport =
            native_markdown_table_intent_step(current, || manager.textViewportLayoutController())?;
        native_markdown_table_intent_step(current, || viewport.layoutViewport())?;
        self.position_find_reveal(context)
    }

    /// 表格高度确定后，按实际segment作一次最小clip调整；不再次调用scrollRangeToVisible。
    pub(in crate::native_surface) fn position_find_reveal(
        &self,
        context: &NativeMarkdownFindRevealContext,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let current = || context.is_current(self);
        let frame = self.find_reveal_segment(context)?;
        let visible = native_markdown_table_intent_step(&current, || self.visibleRect())?;
        let target = native_markdown_minimal_cell_scroll_origin(frame, visible)
            .ok_or(Failure::MissingGeometry)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_geometry("position", &context.probe(), visible, Some(frame), target);
        if (target.x - visible.origin.x).abs() > 0.01 || (target.y - visible.origin.y).abs() > 0.01
        {
            let scroll =
                native_markdown_table_intent_step(&current, || self.enclosingScrollView())?
                    .ok_or(Failure::MissingGeometry)?;
            let clip = native_markdown_table_intent_step(&current, || scroll.contentView())?;
            native_markdown_table_intent_step(&current, || clip.scrollToPoint(target))?;
            native_markdown_table_intent_step(&current, || scroll.reflectScrolledClipView(&clip))?;
            let manager = native_markdown_table_intent_step(&current, || self.textLayoutManager())?
                .ok_or(Failure::MissingGeometry)?;
            let viewport = native_markdown_table_intent_step(&current, || {
                manager.textViewportLayoutController()
            })?;
            native_markdown_table_intent_step(&current, || viewport.layoutViewport())?;
        }
        // clip可能被EOF约束或layout改变；只读重取后认证，不能以请求scroll成功当目标已可见。
        self.verify_find_reveal(context)
    }

    fn verify_find_reveal(
        &self,
        context: &NativeMarkdownFindRevealContext,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let frame = self.find_reveal_segment(context)?;
        let clip =
            native_markdown_table_intent_step(&|| context.is_current(self), || self.visibleRect())?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        trace_find_geometry("verify", &context.probe(), clip, Some(frame), clip.origin);
        if find_segment_is_visible(frame, clip) {
            Ok(())
        } else {
            Err(NativeMarkdownStableTableFailure::AnchorUnstable)
        }
    }

    /// TextKit明确不允许布局时，本次定位失败而非等待下一draw反复尝试。
    pub(in crate::native_surface) fn fail_pending_find_reveal(&self) {
        let Some(context) = self.pending_find_reveal_context() else {
            return;
        };
        if context.is_current(self)
            && let Some(bar) = context.request.bar.upgrade()
            && bar.is_visible()
        {
            bar.status
                .setStringValue(ns_string!("匹配已选中，但当前编辑状态不允许定位"));
        }
        self.remove_find_reveal_if_same(&context.request);
    }

    /// 完成同一个有限executor后才发布成功；失败目标移除，绝不靠draw/timer重试。
    pub(in crate::native_surface) fn finish_pending_find_reveal(&self) {
        let Some(context) = self.pending_find_reveal_context() else {
            return;
        };
        let prepared = context.request.prepared.get();
        let verified = prepared && self.verify_find_reveal(&context).is_ok();
        if context.is_current(self)
            && let Some(bar) = context.request.bar.upgrade()
            && bar.is_visible()
        {
            let text = if verified {
                match context.request.feedback {
                    NativeMarkdownFindRevealFeedback::Found => ns_string!("找到匹配"),
                    NativeMarkdownFindRevealFeedback::ReplacedNext => {
                        ns_string!("已替换，并选中下一个匹配")
                    }
                    NativeMarkdownFindRevealFeedback::ReplacedLast => {
                        ns_string!("已替换，没有更多匹配")
                    }
                }
            } else if !prepared {
                // 看见首个字符不代表整批布局已准备完成；保留原成功门槛并说明真实失败阶段。
                ns_string!("匹配已选中，但当前内容的布局未完成")
            } else {
                // 整批准备完成后仍须通过同票据的实际可见区认证，不能用选中状态替代。
                ns_string!("匹配已选中，但未能完成可见区定位")
            };
            // 此前的Native getters均已结束；setter前纯核票据，随后只移除相同slot。
            if context.is_current(self) {
                bar.status.setStringValue(text);
            }
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        if context.request.completed.get().is_none() {
            // 原成功条件完全不变；反馈setter及其析构已返回后仍核同一原票，不签发后继权限。
            context
                .request
                .completed
                .set(Some(verified && context.is_current(self)));
            context.request.terminal_observer.notify(); // 原反馈与最终权限核对已结束，随后独立事件验证。
        }
        self.remove_find_reveal_if_same(&context.request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "native-markdown-runtime-probe")]
    #[test]
    fn probe_receipt_does_not_promote_prepared_or_removed_requests_to_success() {
        let request = NativeMarkdownFindRevealRequest {
            document: WeakMarkdownDocument::new(),
            bar: std::rc::Weak::new(),
            stamp: FindRevealStamp {
                owner: 1,
                generation: 2,
                length: 3,
                width: 4,
                mode: MarkdownPresentationMode::Source,
                input_epoch: 5,
                table_epoch: 6,
            },
            probe: 1..2,
            feedback: NativeMarkdownFindRevealFeedback::Found,
            visibility: NativeMarkdownFindVisibility::Visible,
            bar_revision: 7,
            selection_capture: None,
            committed: RefCell::new(None),
            ready: Cell::new(true),
            prepared: Cell::new(true),
            completed: Cell::new(None),
            terminal_observer: crate::native_surface::probe::ProbeTerminalObserver::default(),
        };
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        assert!(
            !request
                .observe_probe_terminal(move || callback_calls.set(callback_calls.get() + 1))
                .unwrap()
        );
        // 整批prepared也可能在最终verify失败；缺原终点不能被当成成功。
        assert_eq!(request.probe_completed(), None);
        request.record_probe_removal();
        assert_eq!(request.probe_completed(), Some(false));
        assert_eq!(calls.get(), 1); // 取消 false 唤醒一次原验证，不把 prepared 当成功。
        request.record_probe_removal();
        assert_eq!(request.probe_completed(), Some(false));
        assert_eq!(calls.get(), 1);
        // 真实成功终点在移除之前已签发，正常移除不能把它覆写为失败。
        request.completed.set(Some(true));
        request.record_probe_removal();
        assert_eq!(request.probe_completed(), Some(true));
    }

    #[test]
    fn target_probe_keeps_unicode_scalar_and_eof_without_full_match_layout() {
        assert_eq!(first_match_scalar(12, 100, Some(0x4e2d)), Some(12..13));
        assert_eq!(first_match_scalar(12, 100, Some(0xd83d)), Some(12..14));
        assert_eq!(first_match_scalar(100, 100, None), Some(100..100));
        assert_eq!(first_match_scalar(100, 99, None), None);
        assert_eq!(first_match_scalar(99, 100, Some(0xd83d)), None);
    }

    #[test]
    fn visible_receipt_rejects_old_clip_and_nonfinite_geometry() {
        let clip = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(600.0, 400.0));
        let row = |y| NSRect::new(NSPoint::new(10.0, y), NSSize::new(14.0, 24.0));
        assert!(find_segment_is_visible(row(450.0), clip));
        assert!(!find_segment_is_visible(row(490.0), clip));
        assert!(!find_segment_is_visible(row(700.0), clip));
        assert!(!find_segment_is_visible(row(f64::NAN), clip));
        assert!(!find_segment_is_visible(row(450.0), NSRect::ZERO));
    }
    #[test]
    fn stale_owner_document_query_width_mode_or_slot_never_grants_reveal() {
        let stamp = FindRevealStamp {
            owner: 1,
            generation: 7,
            length: 100,
            width: 900,
            mode: MarkdownPresentationMode::InlinePreview,
            input_epoch: 2,
            table_epoch: 3,
        };
        assert!(find_reveal_permissions(
            stamp, stamp, true, true, true, true
        ));
        for actual in [
            FindRevealStamp { owner: 2, ..stamp },
            FindRevealStamp {
                generation: 8,
                ..stamp
            },
            FindRevealStamp {
                length: 99,
                ..stamp
            },
            FindRevealStamp {
                width: 1120,
                ..stamp
            },
            FindRevealStamp {
                mode: MarkdownPresentationMode::Source,
                ..stamp
            },
            FindRevealStamp {
                input_epoch: 3,
                ..stamp
            },
            FindRevealStamp {
                table_epoch: 4,
                ..stamp
            },
        ] {
            // 每项独立改变都须撤权；不能由其余相同标量或仍活Rc恢复权限。
            assert!(!find_reveal_permissions(
                stamp, actual, true, true, true, true
            ));
        }
        for (slot, foreground, document, visible) in [
            (false, true, true, true),
            (true, false, true, true),
            (true, true, false, true),
            (true, true, true, false),
        ] {
            assert!(!find_reveal_permissions(
                stamp, stamp, slot, foreground, document, visible
            ));
        }
    }
    #[test]
    fn hidden_navigation_preserves_query_without_granting_hidden_mutation() {
        for action in [MarkdownFindAction::Next, MarkdownFindAction::Previous] {
            assert_eq!(
                NativeMarkdownFindVisibility::capture(action, false, false),
                Some(NativeMarkdownFindVisibility::HiddenNavigation)
            );
        }
        for action in [
            MarkdownFindAction::ReplaceNext,
            MarkdownFindAction::ReplaceAll,
            MarkdownFindAction::SelectAll,
            MarkdownFindAction::Hide,
        ] {
            assert_eq!(
                NativeMarkdownFindVisibility::capture(action, false, false),
                None
            );
        }
        for action in [
            MarkdownFindAction::Next,
            MarkdownFindAction::Previous,
            MarkdownFindAction::ReplaceNext,
        ] {
            assert_eq!(
                NativeMarkdownFindVisibility::capture(action, true, true),
                None
            );
        }
        assert!(NativeMarkdownFindVisibility::HiddenNavigation.permits(false, false, 1, 1));
        assert!(!NativeMarkdownFindVisibility::HiddenNavigation.permits(true, false, 1, 1));
        assert!(!NativeMarkdownFindVisibility::Visible.permits(false, false, 1, 1));
        assert!(!NativeMarkdownFindVisibility::HiddenNavigation.permits(false, true, 1, 1));
    }
    #[test]
    fn show_hide_aba_does_not_restore_an_old_navigation_ticket() {
        let original = 40;
        let after_two_transitions = original + 2;
        // Visible→hidden→visible与hidden→visible→hidden虽然回到相同布尔值，票据都已过期。
        assert!(!NativeMarkdownFindVisibility::Visible.permits(
            true,
            false,
            original,
            after_two_transitions
        ));
        assert!(!NativeMarkdownFindVisibility::HiddenNavigation.permits(
            false,
            false,
            original,
            after_two_transitions
        ));
        assert!(NativeMarkdownFindVisibility::Visible.permits(true, false, original, original));
        assert!(
            NativeMarkdownFindVisibility::HiddenNavigation
                .permits(false, false, original, original)
        );
    }
}
