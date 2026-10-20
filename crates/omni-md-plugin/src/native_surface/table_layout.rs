//! Prepares and paints bounded visible table/viewport geometry through public TextKit 2 layout.
//!
//! Queued weak work coalesces by generation and viewport; rendering reads finite immutable snapshots.
//! No RefCell borrow crosses native measurement or painting, and teardown cancels every queued request.

use super::*;

/// 准备结果只在完整矩阵/source覆盖通过后才进入可绘制cache。
type NativeMarkdownStableTableBatch = (
    std::ops::Range<usize>,
    Vec<NativeMarkdownVisibleTableLayout>,
    NativeMarkdownTableLayoutRequestKey,
    std::ops::Range<usize>,
);

/// 一次批次只能有一个连续性来源；Find优先消费自己的目标，不能混用旧顶部锚点。
enum NativeMarkdownTablePreparationTarget<'a> {
    Viewport,
    Mutation(NativeMarkdownTableMutationAnchor),
    Find(&'a NativeMarkdownFindRevealContext),
    Mode(&'a NativeMarkdownModeViewportContext),
}

/// 一个已经可见的完整paragraph起点，不保存原生对象或正文。
struct NativeMarkdownTableViewportAnchor {
    source: std::ops::Range<usize>,
    screen_y: f64,
}

#[cfg(feature = "native-table-edit-diagnostic")]
/// 仅首个Source→Preview批次保留最多32个原Weak，跨两个同步阶段不持有Native强图或正文。
struct NativeMarkdownTablePhaseDiagnosticRow {
    row: usize,
    source: std::ops::Range<usize>,
    fragment: Weak<objc2_app_kit::NSTextLayoutFragment>,
}

/// 模式转换只允许本动作一次受控宽度变化；ready后恢复严格列宽比较。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModeViewportPhase {
    Capturing,
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    Changing,
    Ready(i64),
}

impl ModeViewportPhase {
    /// 同图对照与正式新预览都由原executor消费Ready；捕获阶段没有布局后果。
    fn is_ready(self) -> bool {
        matches!(self, Self::Ready(_))
    }
}

#[derive(Clone, Copy)]
struct ModeViewportAuthority {
    owner: usize,
    mode: MarkdownPresentationMode,
    width: i64,
    input: u64,
    find_input: u64,
    header: u64,
    workspace_input: u64,
}

/// 原票据在首个Native调用之前签发，不从切换后的viewport重新领取源位置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModeViewportStamp {
    owner: usize,
    generation: u64,
    length: usize,
    width: i64,
    before: MarkdownPresentationMode,
    after: MarkdownPresentationMode,
    input: u64,
    find_input: u64,
    header: u64,
    workspace_input: u64,
    selection: MarkdownTextSelection,
}

/// 坐标只取一个完整scalar或EOF caret；不持整段文字、Native range或fragment。
struct ModeViewportAnchor {
    probe: std::ops::Range<usize>,
    source_window: std::ops::Range<usize>,
    screen_y: f64,
}

/// 几何准入同时检查端点，有限分量相加溢出也不能成为滚动请求。
fn mode_rect_is_finite(rect: NSRect) -> bool {
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
        rect.origin.x + rect.size.width,
        rect.origin.y + rect.size.height,
    ]
    .iter()
    .all(|value| value.is_finite())
        && rect.size.width > 0.0
        && rect.size.height > 0.0
}

/// 只在真实fragment与clip的交集内取一个Y，不把clip中心以下的空白吸附成EOF。
fn mode_visible_line_offset(frame: NSRect, visible: NSRect, origin: NSPoint) -> Option<f64> {
    if !mode_rect_is_finite(frame)
        || !mode_rect_is_finite(visible)
        || !origin.x.is_finite()
        || !origin.y.is_finite()
    {
        // 非有限或无面积的矩形没有可查询的实际行，不能给Native传猜测坐标。
        return None;
    }
    let top = frame.origin.y + origin.y;
    let bottom = top + frame.size.height;
    let from = top.max(visible.origin.y);
    let to = bottom.min(visible.origin.y + visible.size.height);
    if ![top, bottom, from, to]
        .iter()
        .all(|value| value.is_finite())
        || from >= to
    {
        // 不相交的已有fragment只由这次有限遍历略过，不要求系统建立离屏布局。
        return None;
    }
    Some(from + (to - from) * 0.5 - top)
}

/// element起点加line局部range只生成候选，后续仍须公开逆查询及真实segment认证。
fn mode_line_source_candidate(
    element: &std::ops::Range<usize>,
    fragment: &std::ops::Range<usize>,
    line: NSRange,
) -> Option<usize> {
    let start = element.start.checked_add(line.location)?;
    let end = start.checked_add(line.length)?;
    (line.length != 0
        && element.start <= fragment.start
        && fragment.end <= element.end
        && fragment.start <= start
        && start < end
        && end <= fragment.end)
        .then_some(start)
}

/// 完整scalar须属于原可见源范围；仅EOF的空caret可恰落在窗口末端。
fn mode_probe_belongs_to_window(
    probe: &std::ops::Range<usize>,
    window: &std::ops::Range<usize>,
) -> bool {
    window.start <= probe.start && probe.start <= probe.end && probe.end <= window.end
}

/// ruler可占clip中的负origin区域；交集只证明正文列完整可见，不产生新的语义宽度。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn mode_content_column_is_visible(bounds: NSRect, visible: NSRect, clip_in_view: NSRect) -> bool {
    if ![bounds, visible, clip_in_view]
        .into_iter()
        .all(mode_rect_is_finite)
    {
        // 任一非法像素范围都不能授权Ready，尤其不能把无穷clip裁成看似有限的正文。
        return false;
    }
    let from = bounds
        .origin
        .x
        .max(visible.origin.x)
        .max(clip_in_view.origin.x);
    let to = (bounds.origin.x + bounds.size.width)
        .min(visible.origin.x + visible.size.width)
        .min(clip_in_view.origin.x + clip_in_view.size.width);
    let top = bounds
        .origin
        .y
        .max(visible.origin.y)
        .max(clip_in_view.origin.y);
    let bottom = (bounds.origin.y + bounds.size.height)
        .min(visible.origin.y + visible.size.height)
        .min(clip_in_view.origin.y + clip_in_view.size.height);
    // 小交集没有整个正文列的权限；0.5pt仅容忍原生几何舍入，不把交集当作新width。
    from < to
        && top < bottom
        && (from - bounds.origin.x).abs() <= 0.5
        && (to - (bounds.origin.x + bounds.size.width)).abs() <= 0.5
}

fn mode_probe_is_visible(frame: NSRect, visible: NSRect) -> bool {
    mode_rect_is_finite(frame)
        && mode_rect_is_finite(visible)
        && native_markdown_minimal_cell_scroll_origin(frame, visible).is_some_and(|origin| {
            (origin.x - visible.origin.x).abs() <= 0.5 && (origin.y - visible.origin.y).abs() <= 0.5
        })
}

/// 尽量保留原screenY，同时只为首scalar的可见性作最小修正，不修改真实选择。
fn mode_probe_scroll_origin(frame: NSRect, visible: NSRect, screen_y: f64) -> Option<NSPoint> {
    if !mode_rect_is_finite(frame)
        || !mode_rect_is_finite(visible)
        || !screen_y.is_finite()
        || screen_y < 0.0
    {
        return None;
    }
    let desired_y = (frame.origin.y - screen_y).max(0.0);
    let desired = NSRect::new(NSPoint::new(visible.origin.x, desired_y), visible.size);
    native_markdown_minimal_cell_scroll_origin(frame, desired)
}

/// cell提交是单个真实old-coordinate替换，故只需映射有限锚点，不缓存changes或正文。
fn mode_map_cell_position(
    position: usize,
    edit: &MarkdownHistoryTextEdit,
    right: bool,
) -> Option<usize> {
    let range = &edit.replaced_utf16_range;
    if range.start > range.end {
        return None;
    }
    let inserted = range
        .len()
        .checked_add_signed(edit.change_in_utf16_length)?;
    if position < range.start {
        return Some(position);
    }
    if position > range.end || (position == range.end && range.start != range.end) {
        return position.checked_add_signed(edit.change_in_utf16_length);
    }
    if right {
        range.start.checked_add(inserted)
    } else {
        Some(range.start)
    }
}

fn mode_stamp_after_owned_cell(
    before: ModeViewportStamp,
    edit: &MarkdownHistoryTextEdit,
    generation: u64,
    length: usize,
) -> Option<ModeViewportStamp> {
    if before.generation.checked_add(1)? != generation
        || before
            .length
            .checked_add_signed(edit.change_in_utf16_length)?
            != length
        || edit.replaced_utf16_range.start > edit.replaced_utf16_range.end
        || edit.replaced_utf16_range.end > before.length
        || edit.selection_after.anchor_utf16 > length
        || edit.selection_after.head_utf16 > length
    {
        return None; // 额外重入编辑、错误receipt或溢出都不能被收编成模式动作自己的提交。
    }
    Some(ModeViewportStamp {
        generation,
        length,
        selection: edit.selection_after,
        ..before
    })
}

/// 每view唯一有界请求；所有原生强claim仅属于同步context，不能跨事件保活正文。
pub(super) struct NativeMarkdownModeViewportRequest {
    document: WeakMarkdownDocument,
    stamp: Cell<ModeViewportStamp>,
    phase: Cell<ModeViewportPhase>,
    anchor: RefCell<Option<ModeViewportAnchor>>,
    prepared: Cell<bool>,
    applied_screen_y: Cell<Option<f64>>,
    /// 完整executor消费结果供原生probe核对；只有有限弱票据可延长到观察栈。
    completed: Cell<Option<bool>>,
    /// 队列在BORROWED期被撤销时只纯撤权，不能遗留Capturing槽阻挡后续布局。
    cancelled: Cell<bool>,
}

/// 和Find各持自己的权限，只共用有限source-probe的真实几何查询。
pub(super) struct NativeMarkdownModeViewportContext {
    request: Rc<NativeMarkdownModeViewportRequest>,
    controller: Retained<NativeMarkdownController>,
    storage: Retained<PieceBackedMarkdownTextStorage>,
}

/// Changing免除的仅是本次ruler输出宽度；文档/输入/窗口代次从未放宽。
fn mode_viewport_phase_permits(
    stamp: ModeViewportStamp,
    phase: ModeViewportPhase,
    mode: MarkdownPresentationMode,
    width: i64,
) -> bool {
    match phase {
        ModeViewportPhase::Capturing => mode == stamp.before && width == stamp.width,
        #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
        ModeViewportPhase::Changing => mode == stamp.before || mode == stamp.after,
        ModeViewportPhase::Ready(expected) => mode == stamp.after && width == expected,
    }
}

fn mode_viewport_authority_permits(
    stamp: ModeViewportStamp,
    phase: ModeViewportPhase,
    actual: ModeViewportAuthority,
) -> bool {
    stamp.owner == actual.owner
        && stamp.input == actual.input
        && stamp.find_input == actual.find_input
        && stamp.header == actual.header
        && stamp.workspace_input == actual.workspace_input
        && mode_viewport_phase_permits(stamp, phase, actual.mode, actual.width)
}

impl NativeMarkdownModeViewportRequest {
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn completed(&self) -> Option<bool> {
        self.completed.get()
    }
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn completion_phase(&self) -> &'static str {
        match self.phase.get() {
            ModeViewportPhase::Capturing => "capturing",
            ModeViewportPhase::Changing => "changing",
            ModeViewportPhase::Ready(_) => "ready",
        }
    }
    fn slot_is_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        !self.cancelled.get()
            && !view.ivars().textkit_graph_teardown.get()
            && self.stamp.get().input == view.ivars().table_input_intent_epoch.get()
            && view
                .ivars()
                .mode_viewport
                .borrow()
                .as_ref()
                .is_some_and(|slot| {
                    std::ptr::eq::<NativeMarkdownModeViewportRequest>(&**slot, self)
                })
    }
    /// 全部为纯Rust检查，允许Active或确切BORROWED，但拒绝Closing/VERIFYING。
    fn is_current(
        &self,
        view: &PieceBackedMarkdownTextView,
        controller: &NativeMarkdownController,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        if !self.authority_current(view, controller, storage) {
            return false;
        }
        let stamp = self.stamp.get();
        let Some(document) = self.document.upgrade() else {
            return false;
        };
        let document = document.borrow();
        document.generation() == stamp.generation
            && document.utf16_len() == stamp.length
            && document.selection() == stamp.selection
    }

    /// 合法自有提交只推进text票据；输入/窗口/slot权限从来不随receipt重新领取。
    fn authority_current(
        &self,
        view: &PieceBackedMarkdownTextView,
        controller: &NativeMarkdownController,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        let stamp = self.stamp.get();
        if !self.slot_is_current(view)
            || !storage.ivars().accepts_edits.get()
            || !native_markdown_controller_has_foreground_authority(controller)
            || !mode_viewport_authority_permits(
                stamp,
                self.phase.get(),
                ModeViewportAuthority {
                    owner: std::ptr::from_ref(controller).addr(),
                    mode: storage.presentation_mode(),
                    width: storage.ivars().table_projection_width_key.get(),
                    input: view.ivars().table_input_intent_epoch.get(),
                    find_input: controller.find_input_intent(),
                    header: controller.ivars().header_layout_revision.get(),
                    workspace_input: controller.workspace_query_intent(),
                },
            )
        {
            return false; // 新输入/模式ABA/窗口resize或关闭只撤权，不借新epoch延续旧目标。
        }
        let identity = storage.ivars().document.borrow().clone();
        if !std::rc::Weak::ptr_eq(&identity, &self.document) {
            return false;
        }
        self.document.upgrade().is_some()
    }
}

impl NativeMarkdownModeViewportContext {
    fn is_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        self.request
            .is_current(view, &self.controller, &self.storage)
    }

    /// 每个新Native原语都由原票据夹住；不能把临时对象返回当授权延长。
    pub(super) fn native_step<T>(
        &self,
        view: &PieceBackedMarkdownTextView,
        work: impl FnOnce() -> T,
    ) -> Result<T, NativeMarkdownStableTableFailure> {
        native_markdown_table_intent_step(&|| self.is_current(view), work)
    }

    fn probe(&self) -> std::ops::Range<usize> {
        self.request
            .anchor
            .borrow()
            .as_ref()
            .expect("captured mode target")
            .probe
            .clone()
    }

    /// 只消费旧mode内本次单cell的真实observed提交；不是把任意后继gen当自有动作。
    pub(super) fn accept_cell_selection(
        &self,
        view: &PieceBackedMarkdownTextView,
        before: MarkdownTextSelection,
        committed: &NativeMarkdownCommittedSelection,
    ) -> bool {
        let stamp = self.request.stamp.get();
        // 这个调用仅来自原选择转换的纯发布回调；root/g/len/输入/宽度/owner 不因同意新选择放宽。
        if self.request.phase.get() != ModeViewportPhase::Capturing
            || stamp.selection != before
            || !self
                .request
                .authority_current(view, &self.controller, &self.storage)
            || !committed.current()
        {
            return false;
        }
        let Some(document) = self.request.document.upgrade() else {
            return false;
        };
        let document = document.borrow();
        if document.generation() != stamp.generation
            || document.utf16_len() != stamp.length
            || document.selection() != committed.selection
        {
            return false;
        }
        self.request.stamp.set(ModeViewportStamp {
            selection: committed.selection,
            ..stamp
        });
        true // source anchor 不移动、不重算；这不是字符修改，也不增加文档代次。
    }

    /// 只消费旧mode内本次单cell的真实observed提交；不是把任意后继gen当自有动作。
    pub(super) fn accept_cell_commit(
        &self,
        view: &PieceBackedMarkdownTextView,
        edit: &MarkdownHistoryTextEdit,
        committed: &NativeMarkdownCommittedSelection,
    ) -> bool {
        if self.request.phase.get() != ModeViewportPhase::Capturing
            || !self
                .request
                .authority_current(view, &self.controller, &self.storage)
            || !committed.current()
        {
            return false;
        }
        let Some(document) = self.request.document.upgrade() else {
            return false;
        };
        let document = document.borrow();
        let Some(next) = mode_stamp_after_owned_cell(
            self.request.stamp.get(),
            edit,
            document.generation(),
            document.utf16_len(),
        ) else {
            return false;
        };
        if document.selection() != next.selection {
            return false;
        }
        let old_ref = self.request.anchor.borrow();
        let Some(old) = old_ref.as_ref() else {
            return false;
        };
        let Some(start) = mode_map_cell_position(old.probe.start, edit, false) else {
            return false;
        };
        let Some(from) = mode_map_cell_position(old.source_window.start, edit, false) else {
            return false;
        };
        let Some(to) = mode_map_cell_position(old.source_window.end, edit, true) else {
            return false;
        };
        let probe = if start == next.length {
            start..start
        } else {
            let Ok(first) = document.utf16_code_unit_at(start) else {
                return false;
            };
            let units = if (0xd800..=0xdbff).contains(&first) {
                2
            } else {
                1
            };
            let Some(end) = start.checked_add(units).filter(|end| *end <= next.length) else {
                return false;
            };
            if document.byte_offset_for_utf16(start).is_err()
                || document.byte_offset_for_utf16(end).is_err()
            {
                return false;
            }
            start..end
        };
        let mapped = ModeViewportAnchor {
            probe,
            source_window: from..to,
            screen_y: old.screen_y,
        };
        drop(old_ref);
        drop(document);
        if !self
            .request
            .authority_current(view, &self.controller, &self.storage)
            || !committed.current()
        {
            return false;
        }
        *self.request.anchor.borrow_mut() = Some(mapped);
        self.request.stamp.set(next); // 唯一纯发布点，之后仍是同slot/原输入票，未复制正文。
        self.is_current(view)
    }

    /// 合法模式写入前进入明确转换阶段，随后只有本次ruler可输出新内容宽度。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn begin_change(&self, view: &PieceBackedMarkdownTextView) -> bool {
        if !self.is_current(view) || self.request.anchor.borrow().is_none() {
            return false;
        }
        self.request.phase.set(ModeViewportPhase::Changing);
        true
    }

    pub(super) fn cancel(&self, view: &PieceBackedMarkdownTextView) {
        view.remove_mode_viewport_if_same(&self.request);
    }

    /// 模式属性已写入后，ruler是同owner的必要UI补偿，不再授权旧source滚动。
    /// 新query可撤定位票，但不能令实际Preview仍留Source ruler；关闭/换doc仍严格拒绝。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn application_current(&self, view: &PieceBackedMarkdownTextView) -> bool {
        !view.ivars().textkit_graph_teardown.get()
            && self.storage.ivars().accepts_edits.get()
            && native_markdown_controller_has_foreground_authority(&self.controller)
            && self.request.stamp.get().owner == std::ptr::from_ref(&*self.controller).addr()
            && self.storage.presentation_mode() == self.request.stamp.get().after
            && std::rc::Weak::ptr_eq(
                &self.storage.ivars().document.borrow(),
                &self.request.document,
            )
            && self.request.document.upgrade().is_some()
    }

    /// 已变更模式不能因定位失败只回退popup；受权时报告独立定位失败。
    pub(super) fn fail(&self, view: &PieceBackedMarkdownTextView) {
        if self.is_current(view)
            && let Some(label) = self.controller.ivars().status_label.get()
        {
            let _ = self.native_step(view, || {
                label.setStringValue(ns_string!("展示模式已切换，但未能保持原可见位置"))
            });
        }
        view.remove_mode_viewport_if_same(&self.request);
    }

    /// 属性和ruler完成后仅粗定位原scalar/EOF，尚未签发Ready时不得消费最终布局。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn coarse_reveal_before_ready(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        if !self.is_current(view)
            || self.request.phase.get() != ModeViewportPhase::Changing
            || self.storage.presentation_mode() != self.request.stamp.get().after
        {
            // 新输入/关闭或未完成模式写入均无滚动权限，不能以必要ruler补偿的较宽权限继续。
            return Err(NativeMarkdownStableTableFailure::InputIntentExpired);
        }
        // 只复制原票中已经认证的scalar/EOF坐标；anchor借用在Native前结束，不取新的可见目标。
        let probe = self.probe();
        self.native_step(view, || {
            // 一次语义粗定位先让TextKit接近原源位置，再由既有executor恢复原screenY并认证。
            // Changing使同步重入executor立即返回；此处不改变选区/焦点，也不保证最终定位成功。
            view.scrollRangeToVisible(NSRange::new(probe.start, probe.len()));
        })
    }

    /// 只封本次受控ruler输出；窗口resize另有原header revision，不可在此重采。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn finish_change(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let bounds = self.native_step(view, || view.bounds())?;
        let width = bounds.size.width;
        let scroll = self
            .native_step(view, || view.enclosingScrollView())?
            .ok_or(Failure::MissingGeometry)?;
        let clip = self.native_step(view, || scroll.contentView())?;
        let clip_bounds = self.native_step(view, || clip.bounds())?;
        // 公开坐标转换保留ruler产生的合法负origin；不硬减某个系统ruler宽度。
        let clip_in_view =
            self.native_step(view, || view.convertRect_fromView(clip_bounds, Some(&clip)))?;
        let visible = self.native_step(view, || view.visibleRect())?;
        drop(clip);
        drop(scroll); // 临时Native claims结束后再纯核原票，不能在析构后领取新代次。
        if !self.is_current(view)
            || self.storage.presentation_mode() != self.request.stamp.get().after
            || !mode_content_column_is_visible(bounds, visible, clip_in_view)
            || self.storage.ivars().table_projection_width_key.get()
                != native_markdown_layout_metric(width)
        {
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 只输出原生调用已经取得的坐标；交集未覆盖正文仍失败，不重新签发Ready票。
            eprintln!(
                "native_mode_completion_failure stage=finish_change bounds={bounds:?} clip_in_view={clip_in_view:?} visible={visible:?}"
            );
            return Err(Failure::InputIntentExpired);
        }
        self.request
            .phase
            .set(ModeViewportPhase::Ready(native_markdown_layout_metric(
                width,
            )));
        self.is_current(view)
            .then_some(())
            .ok_or(Failure::InputIntentExpired)
    }
}

impl PieceBackedMarkdownTextView {
    /// 正式调用者借出现有controller/storage，首个retain之前即固定原owner与所有输入权限。
    pub(super) fn capture_mode_viewport_transition(
        &self,
        controller: &Retained<NativeMarkdownController>,
        storage: &Retained<PieceBackedMarkdownTextStorage>,
        after: MarkdownPresentationMode,
    ) -> Result<NativeMarkdownModeViewportContext, NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        if let Some(request) = self.authorized_mode_action(controller, storage, after) {
            // 正式popup已在首getter之前捕获；此处只延续原弱票，不再次读取source或epoch。
            let current = || request.is_current(self, controller, storage);
            let controller = native_markdown_table_intent_step(&current, || controller.clone())?;
            let storage = native_markdown_table_intent_step(&current, || storage.clone())?;
            return Ok(NativeMarkdownModeViewportContext {
                request,
                controller,
                storage,
            });
        }
        let identity = storage.ivars().document.borrow().clone();
        let document = identity.upgrade().ok_or(Failure::InputIntentExpired)?;
        let stamp = {
            let document = document.borrow();
            ModeViewportStamp {
                owner: std::ptr::from_ref(&**controller).addr(),
                generation: document.generation(),
                length: document.utf16_len(),
                selection: document.selection(),
                width: storage.ivars().table_projection_width_key.get(),
                before: storage.presentation_mode(),
                after,
                input: self.ivars().table_input_intent_epoch.get(),
                find_input: controller.find_input_intent(),
                header: controller.ivars().header_layout_revision.get(),
                workspace_input: controller.workspace_query_intent(),
            }
        };
        drop(document);
        let request = Rc::new(NativeMarkdownModeViewportRequest {
            document: identity,
            stamp: Cell::new(stamp),
            phase: Cell::new(ModeViewportPhase::Capturing),
            anchor: RefCell::new(None),
            prepared: Cell::new(false),
            applied_screen_y: Cell::new(None),
            completed: Cell::new(None),
            cancelled: Cell::new(false),
        });
        // 旧slot只有Rust Weak/标量；先换出再drop，模式ABA不能复活旧局部目标。
        let previous = self
            .ivars()
            .mode_viewport
            .replace(Some(Rc::clone(&request)));
        drop(previous);
        let current = || request.is_current(self, controller, storage);
        let result = (|| {
            let controller = native_markdown_table_intent_step(&current, || controller.clone())?;
            let storage = native_markdown_table_intent_step(&current, || storage.clone())?;
            let context = NativeMarkdownModeViewportContext {
                request: Rc::clone(&request),
                controller,
                storage,
            };
            self.capture_mode_viewport_anchor(&context)?;
            Ok(context)
        })();
        if result.is_err() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 只报告原调用已经取得的失败类型；不额外查询Native，以免诊断先行建立缺失布局。
            eprintln!(
                "native_mode_capture_failure stage=transaction error={:?}",
                result.as_ref().err()
            );
            self.remove_mode_viewport_if_same(&request);
        }
        result
    }

    /// 仅有完整Capturing票可进入正式模式动作；Ready旧定位和空/过期slot都不算。
    pub(super) fn authorized_mode_action(
        &self,
        controller: &NativeMarkdownController,
        storage: &PieceBackedMarkdownTextStorage,
        after: MarkdownPresentationMode,
    ) -> Option<Rc<NativeMarkdownModeViewportRequest>> {
        let request = self
            .ivars()
            .mode_viewport
            .borrow()
            .as_ref()
            .map(Rc::clone)?;
        (request.phase.get() == ModeViewportPhase::Capturing
            && request.stamp.get().after == after
            && request.anchor.borrow().is_some()
            && request.is_current(self, controller, storage))
        .then_some(request)
    }

    /// 单次遍历至多32个已布局fragment，只为首条屏内实际行生成一个源候选。
    fn visible_mode_line_candidate(
        &self,
        context: &NativeMarkdownModeViewportContext,
        source_window: &std::ops::Range<usize>,
        visible: NSRect,
        origin: NSPoint,
    ) -> Result<usize, NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let current = || context.is_current(self);
        native_markdown_bounded_table_layout_range(source_window, source_window)
            .ok_or(Failure::CoverageOutsideBudget)?;
        let manager = context
            .native_step(self, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let content = context
            .native_step(self, || manager.textContentManager())?
            .ok_or(Failure::MissingGeometry)?;
        if context.native_step(self, || content.hasEditingTransaction())? {
            // 活编辑组内不枚举段落；原Mode动作失败，不要求系统先完成全文布局。
            return Err(Failure::MissingGeometry);
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range = context.native_step(self, || provider.documentRange())?;
        let document_start = context.native_step(self, || document_range.location())?;
        let offset =
            isize::try_from(source_window.start).map_err(|_| Failure::InvalidSourceAssociation)?;
        let start = context
            .native_step(self, || {
                content.locationFromLocation_withOffset(&document_start, offset)
            })?
            .ok_or(Failure::InvalidSourceAssociation)?;
        let source_range = |range: &NSTextRange| -> Result<std::ops::Range<usize>, Failure> {
            let from = context.native_step(self, || range.location())?;
            let to = context.native_step(self, || range.endLocation())?;
            let from = usize::try_from(context.native_step(self, || {
                content.offsetFromLocation_toLocation(&document_start, &from)
            })?)
            .map_err(|_| Failure::InvalidSourceAssociation)?;
            let to = usize::try_from(context.native_step(self, || {
                content.offsetFromLocation_toLocation(&document_start, &to)
            })?)
            .map_err(|_| Failure::InvalidSourceAssociation)?;
            if from > to || to > context.request.stamp.get().length {
                // 原生范围须属于同一正文的全局UTF16；非法关联不能参与候选算术。
                return Err(Failure::InvalidSourceAssociation);
            }
            Ok(from..to)
        };
        let visited = Cell::new(0usize);
        let candidate = Cell::new(None);
        let failure = Cell::new(None);
        let block =
            block2::RcBlock::new(|pointer: NonNull<objc2_app_kit::NSTextLayoutFragment>| {
                visited.set(visited.get() + 1); // 第32个仍无候选即停止，不请求第33个fragment。
                let result = (|| -> Result<Option<usize>, Failure> {
                    if !current() {
                        // 枚举回调也可位于Native重入栈，先拒绝原输入已撤销的目标。
                        return Err(Failure::InputIntentExpired);
                    }
                    // SAFETY: 同步公开枚举仅在callback期间借出活fragment，未保存该指针或强Native资源。
                    let fragment = unsafe { pointer.as_ref() };
                    if context.native_step(self, || fragment.state())?
                        != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
                    {
                        // 只有实际行布局可用才能调用单行getter；不让它补建估计fragment。
                        return Err(Failure::MissingGeometry);
                    }
                    let actual_manager = context
                        .native_step(self, || fragment.textLayoutManager())?
                        .ok_or(Failure::MissingGeometry)?;
                    if !std::ptr::eq(&*actual_manager, &*manager) {
                        // 其他manager的fragment不能用当前provider解释源位置。
                        return Err(Failure::InvalidSourceAssociation);
                    }
                    let frame = context.native_step(self, || fragment.layoutFragmentFrame())?;
                    let Some(y) = mode_visible_line_offset(frame, visible, origin) else {
                        // 这次有限枚举可略过不相交的旧片段，仍不主动布局其内容。
                        return Ok(None);
                    };
                    let Some(line) = context.native_step(self, || {
                        fragment.textLineFragmentForVerticalOffset_requiresExactMatch(y, false)
                    })?
                    else {
                        // 此fragment交集没有实际行，只继续这一次已有fragment遍历，不重复同一查询。
                        return Ok(None);
                    };
                    let bounds = context.native_step(self, || line.typographicBounds())?;
                    let line_range = context.native_step(self, || line.characterRange())?;
                    let line_top = origin.y + frame.origin.y + bounds.origin.y;
                    let line_bottom = line_top + bounds.size.height;
                    if ![line_top, line_bottom, bounds.origin.x, bounds.size.width]
                        .iter()
                        .all(|value| value.is_finite())
                        || bounds.size.height <= 0.0
                        || line_top < visible.origin.y - 0.5
                        || line_bottom > visible.origin.y + visible.size.height + 0.5
                        || line_range.length == 0
                    {
                        // 顶部残行/底部残行或EOF空行不能假称完整字形可见；只考察后续实际fragment。
                        return Ok(None);
                    }
                    let element = context
                        .native_step(self, || fragment.textElement())?
                        .ok_or(Failure::InvalidSourceAssociation)?;
                    let element_content = context
                        .native_step(self, || element.textContentManager())?
                        .ok_or(Failure::InvalidSourceAssociation)?;
                    if !std::ptr::eq(&*element_content, &*content) {
                        // element允许迁移manager，故在使用其range前明确拒绝迁移后的关联。
                        return Err(Failure::InvalidSourceAssociation);
                    }
                    let element_range = context
                        .native_step(self, || element.elementRange())?
                        .ok_or(Failure::InvalidSourceAssociation)?;
                    let fragment_range = context.native_step(self, || fragment.rangeInElement())?;
                    let element_source = source_range(&element_range)?;
                    let fragment_source = source_range(&fragment_range)?;
                    let Some(offset) =
                        mode_line_source_candidate(&element_source, &fragment_source, line_range)
                    else {
                        // line局部范围不能无条件加fragment起点；只接受element原点下的合法嵌套候选。
                        return Err(Failure::InvalidSourceAssociation);
                    };
                    // 原文字位置getter会为巨段落重新构造全文属性值；改核同一已布局行的元数据。
                    let line_source = offset
                        ..offset
                            .checked_add(line_range.length)
                            .ok_or(Failure::InvalidSourceAssociation)?;
                    super::line_source_metadata::verify_native_markdown_line_metadata(
                        super::line_source_metadata::NativeMarkdownLineMetadataRequest {
                            fragment,
                            line: &line,
                            content: &content,
                            document_start: &document_start,
                            document_length: context.request.stamp.get().length,
                            element_source: &element_source,
                            source: &line_source,
                            local_range: line_range,
                            bounds,
                        },
                        &current,
                    )
                    .map_err(|error| match error {
                        super::line_source_metadata::NativeMarkdownLineMetadataError::Revoked => {
                            Failure::InputIntentExpired
                        }
                        super::line_source_metadata::NativeMarkdownLineMetadataError::Source => {
                            Failure::InvalidSourceAssociation
                        }
                        super::line_source_metadata::NativeMarkdownLineMetadataError::Geometry => {
                            Failure::UnstableFrames
                        }
                    })?;
                    let final_range = context.native_step(self, || fragment.rangeInElement())?;
                    let final_source = source_range(&final_range)?;
                    if context.native_step(self, || line.characterRange())? != line_range
                        || context.native_step(self, || line.typographicBounds())? != bounds
                        || context.native_step(self, || fragment.layoutFragmentFrame())? != frame
                        || context.native_step(self, || fragment.state())?
                            != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
                        || final_source != fragment_source
                    {
                        // 公开逆查只需同源行值，不要求getter返回相同对象；变化中的行/源/几何不能发布。
                        return Err(Failure::UnstableFrames);
                    }
                    Ok(Some(offset))
                })(); // 全部临时Native claims在这里结束，之后还要纯核原票。
                if !current() {
                    failure.set(Some(Failure::InputIntentExpired));
                    return objc2::runtime::Bool::NO;
                }
                match result {
                    Ok(Some(offset)) => {
                        // 首个实际行候选一旦取得立即停止；最终scalar/segment认证失败也不再另找候选。
                        candidate.set(Some(offset));
                        objc2::runtime::Bool::NO
                    }
                    Ok(None) if visited.get() < 32 => objc2::runtime::Bool::YES,
                    Ok(None) => {
                        // 容量耗尽不等于没有可见正文，必须保留明确失败而非截断当成功。
                        failure.set(Some(Failure::CoverageOutsideBudget));
                        objc2::runtime::Bool::NO
                    }
                    Err(error) => {
                        // 原生关联/权限失败保留首因，停止本次枚举，不触发任何重试或ensure。
                        failure.set(Some(error));
                        objc2::runtime::Bool::NO
                    }
                }
            });
        let end = context.native_step(self, || {
            manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                Some(&start),
                objc2_app_kit::NSTextLayoutFragmentEnumerationOptions::empty(),
                &block,
            )
        })?;
        drop(end);
        drop(block); // 不保留callback或原生行；caller的外层native_step覆盖本函数全部局部析构。
        if !current() {
            return Err(Failure::InputIntentExpired);
        }
        if let Some(error) = failure.get() {
            return Err(error);
        }
        candidate.get().ok_or(Failure::AnchorUnavailable)
    }

    /// 只取已存在的可见几何；不为capture主动layout或使用会切到TK1的插入点查询。
    fn capture_mode_viewport_anchor(
        &self,
        context: &NativeMarkdownModeViewportContext,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        #[cfg(feature = "native-markdown-runtime-probe")]
        // 只复制原stamp的before值作诊断范围；Native调用顺序与原捕获权限完全不变。
        let _reentry_source_scope = super::viewport_reentry_diagnostic::source_anchor_scope(
            context.request.stamp.get().before == MarkdownPresentationMode::Source,
        );
        use NativeMarkdownStableTableFailure as Failure;
        let current = || context.is_current(self);
        let viewport = context.native_step(self, || self.bounded_paint_viewport())?;
        let Some((visible, origin)) = viewport else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 挂载/可见窗口/有限clip准入合并在原helper中；此处不重读getter猜具体子原因。
            eprintln!("native_mode_capture_failure stage=bounded_viewport");
            return Err(Failure::AnchorUnavailable);
        };
        let stamp = context.request.stamp.get();
        let length = stamp.length;
        let source_window =
            context.native_step(self, || self.read_visible_utf16_window(visible, length))?;
        if length != 0 && source_window.is_empty() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 非空正文没有真实可见源范围时严格失败，不用caret或固定0代替原视口。
            eprintln!(
                "native_mode_capture_failure stage=visible_source length={length} visible={visible:?}"
            );
            return Err(Failure::AnchorUnavailable);
        }
        let scalar_probe = |offset: usize| -> Result<std::ops::Range<usize>, Failure> {
            let document = context
                .request
                .document
                .upgrade()
                .ok_or(Failure::InputIntentExpired)?;
            let document = document.borrow();
            if !current() || offset > length {
                return Err(Failure::InputIntentExpired);
            }
            if offset == length {
                return Ok(offset..offset);
            } // EOF caret没有额外字符，不回退到前一段。
            let first = document
                .utf16_code_unit_at(offset)
                .map_err(|_| Failure::InvalidSourceAssociation)?;
            let units = if (0xd800..=0xdbff).contains(&first) {
                2
            } else {
                1
            };
            let end = offset
                .checked_add(units)
                .filter(|end| *end <= length)
                .ok_or(Failure::InvalidSourceAssociation)?;
            document
                .byte_offset_for_utf16(offset)
                .map_err(|_| Failure::InvalidSourceAssociation)?;
            document
                .byte_offset_for_utf16(end)
                .map_err(|_| Failure::InvalidSourceAssociation)?;
            Ok(offset..end)
        };
        let mut anchor = None;
        if let Ok(selection) = context
            .native_step(self, || self.primary_text_selection())?
            .inspect_err(|_error| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                // 只报告原查询已有的选择失败；不为capture另读caret或把canonical当作已显示的Native选择。
                eprintln!("native_mode_capture_failure stage=primary_selection error={_error}");
            })
        {
            // 主选择已由公开TK2信息证明；这里只取其范围起点用于几何，不改anchor/head方向。
            let offset = selection
                .selection
                .anchor_utf16
                .min(selection.selection.head_utf16);
            let probe = scalar_probe(offset).inspect_err(|_error| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                // 仅记录原选择候选的坐标；不输出码元或重新读取正文来诊断边界。
                eprintln!("native_mode_capture_failure stage=selection_scalar error={_error:?} candidate={offset} source={}..{} length={length}", source_window.start, source_window.end);
            })?;
            if let Ok(frame) = self.source_probe_segment(&probe, &current).inspect_err(|_error| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                // 同一个实际主scalar的已有失败归因，不能让后续line候选掩盖其资源/几何来源。
                eprintln!("native_mode_capture_failure stage=primary_segment error={_error:?} candidate={}..{} source={}..{}", probe.start, probe.end, source_window.start, source_window.end);
            }) {
                let clip = context.native_step(self, || self.visibleRect())?;
                if mode_probe_is_visible(frame, clip) {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    native_markdown_trace! {
                        // 只记录原选择已经取得的scalar/屏幕坐标，不为日志再查selection或segment。
                        eprintln!("native_mode_viewport stage=capture kind=selection before={:?} after={:?} probe={}..{} source={}..{} screen_y={} old_width={} frame={frame:?} visible={clip:?}", stamp.before, stamp.after, probe.start, probe.end, source_window.start, source_window.end, frame.origin.y - clip.origin.y, stamp.width);
                    }
                    anchor = Some(ModeViewportAnchor {
                        probe,
                        source_window: source_window.clone(),
                        screen_y: frame.origin.y - clip.origin.y,
                    });
                }
            }
        }
        if !current() {
            return Err(Failure::InputIntentExpired);
        }
        if anchor.is_none() && length == 0 {
            // 空文档没有可丢失的源文字；保留唯一EOF位置，不虚构一个paragraph。
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_trace! {
                // EOF空态沿同一开关输出常量收据，不补建段落或布局。
                eprintln!("native_mode_viewport stage=capture kind=empty before={:?} after={:?} probe=0..0 source=0..0 screen_y=0 old_width={}", stamp.before, stamp.after, stamp.width);
            }
            anchor = Some(ModeViewportAnchor {
                probe: 0..0,
                source_window: 0..0,
                screen_y: 0.0,
            });
        }
        if anchor.is_none() {
            // 离屏caret不能拉回用户；从原有限窗口的已有LayoutAvailable片段只选首条实际可见行。
            // 单次空options遍历不建立新布局，不要求整个paragraph的起点可见，也不信点吸附的EOF。
            let offset = context.native_step(self, || {
                self.visible_mode_line_candidate(context, &source_window, visible, origin)
            })??;
            if offset < source_window.start || offset > source_window.end {
                // 行关联候选仍须属于原窗口，不能用Native返回值扩张这次准备的源范围。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_mode_capture_failure stage=line_outside_window candidate={offset} visible={visible:?} source={}..{} length={length}",
                    source_window.start, source_window.end
                );
                return Err(Failure::InvalidSourceAssociation);
            }
            let probe = scalar_probe(offset).inspect_err(|_error| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                // 候选已属原窗口，失败继续区分为原正文的scalar转换，不补领新doc身份。
                eprintln!("native_mode_capture_failure stage=line_scalar error={_error:?} candidate={offset} source={}..{} length={length}", source_window.start, source_window.end);
            })?;
            if !mode_probe_belongs_to_window(&probe, &source_window) {
                // offset等于末端不代表后一个非空scalar仍属原窗口，不能跨边界扩张准备权限。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_mode_capture_failure stage=line_scalar_outside_window candidate={}..{} source={}..{} length={length}",
                    probe.start, probe.end, source_window.start, source_window.end
                );
                return Err(Failure::InvalidSourceAssociation);
            }
            let frame = self.source_probe_segment(&probe, &current).inspect_err(|_error| {
                #[cfg(feature = "native-markdown-runtime-probe")]
                // scalar已通过原窗口和正文校验，继续区分系统location/range或segment构造失败。
                eprintln!("native_mode_capture_failure stage=line_segment error={_error:?} candidate={}..{} source={}..{} length={length}", probe.start, probe.end, source_window.start, source_window.end);
            })?;
            let clip = context.native_step(self, || self.visibleRect())?;
            if clip != visible || !mode_probe_is_visible(frame, clip) {
                // 同票仍须实际首scalar完整可见；不能把点命中结果当作字形认证或接受已移动的旧视口。
                return Err(Failure::AnchorUnavailable);
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_trace! {
                // 已完成有限line/scalar关联，诊断仅消费现有值，不能把它升级为一次新定位。
                eprintln!("native_mode_viewport stage=capture kind=line before={:?} after={:?} probe={}..{} source={}..{} screen_y={} old_width={} frame={frame:?} visible={clip:?}", stamp.before, stamp.after, probe.start, probe.end, source_window.start, source_window.end, frame.origin.y - clip.origin.y, stamp.width);
            }
            anchor = Some(ModeViewportAnchor {
                probe,
                source_window,
                screen_y: frame.origin.y - clip.origin.y,
            });
        }
        if !current() {
            return Err(Failure::InputIntentExpired);
        }
        *context.request.anchor.borrow_mut() = anchor; // 仅移动有限Rust值，无Native析构跨借用。
        Ok(())
    }

    /// 新输入、编辑/关闭与另一模式动作只撤销旧slot，不排队恢复或重放。
    pub(super) fn cancel_pending_mode_viewport(&self) {
        let previous = self.ivars().mode_viewport.borrow_mut().take();
        if let Some(previous) = previous.as_ref() {
            previous.completed.set(Some(false));
        }
        drop(previous);
    }

    pub(super) fn cancel_mode_viewport_for_width(&self, width: f64) {
        let metric = native_markdown_layout_metric(width);
        let expired = self
            .ivars()
            .mode_viewport
            .borrow()
            .as_ref()
            .is_some_and(|request| match request.phase.get() {
                ModeViewportPhase::Capturing => request.stamp.get().width != metric,
                #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
                ModeViewportPhase::Changing => false, // 原窗口revision仍固定，本动作ruler可输出一次新宽。
                ModeViewportPhase::Ready(expected) => expected != metric,
            });
        if expired {
            self.cancel_pending_mode_viewport();
        }
    }

    fn remove_mode_viewport_if_same(&self, request: &Rc<NativeMarkdownModeViewportRequest>) {
        let previous = {
            let mut slot = self.ivars().mode_viewport.borrow_mut();
            if slot.as_ref().is_some_and(|slot| Rc::ptr_eq(slot, request)) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(previous) = previous.as_ref()
            && previous.completed.get().is_none()
        {
            previous.completed.set(Some(false));
        }
        drop(previous);
    }

    /// 异步槽不持Native；升级只发生在payload租约内，之后核原票，绝不领取新epoch。
    fn pending_mode_viewport_context(&self) -> Option<NativeMarkdownModeViewportContext> {
        let request = self
            .ivars()
            .mode_viewport
            .borrow()
            .as_ref()
            .map(Rc::clone)?;
        if !request.phase.get().is_ready() {
            return None;
        }
        if !request.slot_is_current(self) {
            self.remove_mode_viewport_if_same(&request);
            return None; // 首个Native weak.load之前先拒绝已封闭或新物理输入的旧目标。
        }
        let Some(controller) = self.ivars().controller.get().and_then(Weak::load) else {
            self.remove_mode_viewport_if_same(&request);
            return None;
        };
        let stamp = request.stamp.get();
        if !request.slot_is_current(self)
            || stamp.owner != std::ptr::from_ref(&*controller).addr()
            || stamp.find_input != controller.find_input_intent()
            || stamp.header != controller.ivars().header_layout_revision.get()
            || stamp.workspace_input != controller.workspace_query_intent()
            || !native_markdown_controller_has_foreground_authority(&controller)
        {
            self.remove_mode_viewport_if_same(&request);
            return None; // 一个weak升级不能把较新controller输入权限贴回旧source。
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            self.remove_mode_viewport_if_same(&request);
            return None;
        };
        let context = NativeMarkdownModeViewportContext {
            request,
            controller,
            storage,
        };
        if context.is_current(self) {
            Some(context)
        } else {
            self.remove_mode_viewport_if_same(&context.request);
            None
        }
    }

    /// 一个目标只在该次完整executor结束时消费，不依赖下一draw或稳定化重试。
    fn finish_pending_mode_viewport(&self, layout_allowed: bool) {
        let Some(context) = self.pending_mode_viewport_context() else {
            return;
        };
        let prepared = context.request.prepared.get();
        // 保持原短路顺序：只有允许布局且整批准备完成才执行原来的最终可见性查询。
        let verification =
            (layout_allowed && prepared).then(|| self.verify_mode_viewport(&context));
        if !matches!(verification, Some(Ok(()))) {
            #[cfg(feature = "native-markdown-runtime-probe")]
            // 原请求失败不能由slot已移除推成成功；记录已有许可/准备/认证结果以区分阶段。
            eprintln!(
                "native_mode_completion_failure stage=executor layout_allowed={layout_allowed} prepared={prepared} error={:?}",
                verification.and_then(Result::err)
            );
            context.fail(self);
        } else {
            context.request.completed.set(Some(true));
            self.remove_mode_viewport_if_same(&context.request);
        }
    }

    /// Source或普通正文仍只准备目标scalar/EOF，不把旧完整paragraph交给ensure。
    fn prepare_mode_viewport_without_tables(
        &self,
        context: &NativeMarkdownModeViewportContext,
        current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        if context.request.stamp.get().length == 0 {
            return self.position_mode_viewport(context, None);
        }
        self.ensure_stable_table_source_range(&context.probe(), current)?;
        let manager = native_markdown_table_intent_step(current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let viewport =
            native_markdown_table_intent_step(current, || manager.textViewportLayoutController())?;
        native_markdown_table_intent_step(current, || viewport.layoutViewport())?;
        self.position_mode_viewport(context, None)
    }

    /// 目标高度确定后恢复同一源scalar的屏幕位置；只用公开clip约束处理EOF边界。
    fn position_mode_viewport(
        &self,
        context: &NativeMarkdownModeViewportContext,
        tables: Option<&[NativeMarkdownVisibleTableLayout]>,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        if context.request.stamp.get().length == 0 {
            context.request.applied_screen_y.set(Some(0.0));
            return Ok(());
        }
        let current = || context.is_current(self);
        let probe = context.probe();
        #[cfg(feature = "native-raster-surface-probe")]
        {
            let raster_active = self.ivars().raster_surface.borrow().is_some();
            if raster_active {
                // 光栅正文不安装系统 glyph surface；先用原全局源位置更新公开 viewport 锚点，
                // 不能把尚未完整布局的绝对 y 当作下一次 viewport 的稳定文档坐标。
                self.relocate_mode_viewport(context, &probe)?;
            }
        }
        let frame = match tables
            .map(|tables| native_markdown_table_scalar_frame(tables, &probe))
            .transpose()?
            .flatten()
        {
            Some(frame) => frame,
            None => self.source_probe_segment(&probe, &current)?,
        };
        let visible = context.native_step(self, || self.visibleRect())?;
        let screen_y = context
            .request
            .anchor
            .borrow()
            .as_ref()
            .ok_or(Failure::AnchorUnavailable)?
            .screen_y;
        let target =
            mode_probe_scroll_origin(frame, visible, screen_y).ok_or(Failure::AnchorUnstable)?;
        let scroll = context
            .native_step(self, || self.enclosingScrollView())?
            .ok_or(Failure::MissingGeometry)?;
        let clip = context.native_step(self, || scroll.contentView())?;
        let bounds = context.native_step(self, || clip.bounds())?;
        let constrained = context.native_step(self, || {
            clip.constrainBoundsRect(NSRect::new(target, bounds.size))
        })?;
        if !mode_rect_is_finite(constrained) {
            return Err(Failure::AnchorUnstable);
        }
        let applied_screen_y = frame.origin.y - constrained.origin.y;
        context.request.applied_screen_y.set(Some(applied_screen_y));
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_trace! {
            // 滚动前frame与系统约束都已经读取；不查询后继布局来影响此次请求。
            eprintln!("native_mode_viewport stage=position probe={}..{} frame={frame:?} visible={visible:?} old_screen_y={screen_y} target={target:?} clip_bounds={bounds:?} constrained={constrained:?} applied_screen_y={applied_screen_y}", probe.start, probe.end);
        }
        if (constrained.origin.y - bounds.origin.y).abs() > 0.01
            || (constrained.origin.x - bounds.origin.x).abs() > 0.01
        {
            context.native_step(self, || clip.scrollToPoint(constrained.origin))?;
            context.native_step(self, || scroll.reflectScrolledClipView(&clip))?;
            let manager = context
                .native_step(self, || self.textLayoutManager())?
                .ok_or(Failure::MissingGeometry)?;
            let viewport = context.native_step(self, || manager.textViewportLayoutController())?;
            context.native_step(self, || viewport.layoutViewport())?;
        }
        drop(clip);
        drop(scroll);
        if tables.is_some() {
            Ok(()) // 自有scroll可能兑现新行高；原有第二矩阵/最多两批随后以真实cell glyph完成认证。
        } else {
            self.verify_mode_viewport(context)
        }
    }

    /// 只移动同一次模式动作的真实 Native 源锚点，不扩大 ensure 范围或重新签发输入权限。
    fn relocate_mode_viewport(
        &self,
        context: &NativeMarkdownModeViewportContext,
        probe: &std::ops::Range<usize>,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let manager = context
            .native_step(self, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let content = context
            .native_step(self, || manager.textContentManager())?
            .ok_or(Failure::MissingGeometry)?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document = context.native_step(self, || provider.documentRange())?;
        let start = context.native_step(self, || document.location())?;
        let offset = isize::try_from(probe.start).map_err(|_| Failure::InvalidSourceAssociation)?;
        let location = context
            .native_step(self, || {
                content.locationFromLocation_withOffset(&start, offset)
            })?
            .ok_or(Failure::InvalidSourceAssociation)?;
        if context.native_step(self, || {
            content.offsetFromLocation_toLocation(&start, &location)
        })? != offset
        {
            return Err(Failure::InvalidSourceAssociation); // 原 provider 端点不能精确往返时不移动窗口。
        }
        let viewport = context.native_step(self, || manager.textViewportLayoutController())?;
        let suggested =
            context.native_step(self, || viewport.relocateViewportToTextLocation(&location))?;
        if !suggested.is_finite() {
            return Err(Failure::AnchorUnstable); // 建议点只是系统返回值，非法值不能当作已完成布局。
        }
        // relocate只移动TextKit内部的源锚；必须把其建议位置同步到同一个真实clip，
        // 再让viewport布局，避免内部新锚与外部旧clip同时参与下一次位置估算。
        let origin = context.native_step(self, || self.textContainerOrigin())?;
        let visible = context.native_step(self, || self.visibleRect())?;
        let target_y = suggested + origin.y;
        if !target_y.is_finite() || !visible.origin.x.is_finite() {
            return Err(Failure::AnchorUnstable); // 坐标转换失败不改用另一位置或另一滚动API。
        }
        let scroll = context
            .native_step(self, || self.enclosingScrollView())?
            .ok_or(Failure::MissingGeometry)?;
        let clip = context.native_step(self, || scroll.contentView())?;
        let document_view = context
            .native_step(self, || clip.documentView())?
            .ok_or(Failure::MissingGeometry)?;
        let actual_manager = context
            .native_step(self, || viewport.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let original_view: &objc2_app_kit::NSView = self;
        if !std::ptr::eq::<objc2_app_kit::NSView>(&*document_view, original_view)
            || !std::ptr::eq::<objc2_app_kit::NSTextLayoutManager>(&*actual_manager, &*manager)
        {
            return Err(Failure::InvalidSourceAssociation); // 原建议锚不能迁移已换图的document view。
        }
        context.native_step(self, || {
            clip.scrollToPoint(NSPoint::new(visible.origin.x, target_y.max(0.0)))
        })?;
        context.native_step(self, || scroll.reflectScrolledClipView(&clip))?;
        // 这一遍只完成真实粗迁移；随后仍以实际scalar frame恢复原screenY并严格认证。
        // 它不是认证失败后的重试，也不使用suggested数值伪造目标的实际矩形。
        context.native_step(self, || viewport.layoutViewport())?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_trace! {
            eprintln!("native_mode_viewport stage=relocate probe={}..{} suggested_y={suggested} clip_target_y={target_y} formal=false", probe.start, probe.end);
        }
        Ok(()) // 实际最终 frame/clip 与原 screen_y 仍由原 position/verify 全部认证。
    }

    fn verify_mode_viewport(
        &self,
        context: &NativeMarkdownModeViewportContext,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        if !context.is_current(self) {
            return Err(Failure::InputIntentExpired);
        }
        if context.request.stamp.get().length == 0 {
            return Ok(());
        }
        let probe = context.probe();
        let frame = self.source_probe_segment(&probe, &|| context.is_current(self))?;
        let visible = context.native_step(self, || self.visibleRect())?;
        let expected = context
            .request
            .applied_screen_y
            .get()
            .ok_or(Failure::AnchorUnavailable)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_trace! {
            // 最终几何来自原有认证getter；prepared只读现成标量，区分定位内与整批完成后的复核。
            eprintln!("native_mode_viewport stage=verify probe={}..{} frame={frame:?} visible={visible:?} expected={expected} actual_screen_y={} delta={} prepared={}", probe.start, probe.end, frame.origin.y - visible.origin.y, frame.origin.y - visible.origin.y - expected, context.request.prepared.get());
        }
        if !mode_probe_is_visible(frame, visible)
            || (frame.origin.y - visible.origin.y - expected).abs() > 0.5
        {
            return Err(Failure::AnchorUnstable);
        }
        Ok(())
    }

    /// 修改前只读已有布局；不layout、不持原生对象，也不把旧行高授权给新generation。
    pub(super) fn capture_table_mutation_anchor(
        &self,
    ) -> Option<NativeMarkdownTableMutationAnchor> {
        // Undo等直接调用者也必须让局部fragment/native claims在租约内结束。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        // 最早固定意图，诊断/getter内重入也不能把旧capture改贴到新事件。
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        self.table_edit_diagnostic("capture_before_mutation");
        let storage = self.ivars().text_storage.load()?;
        let key = self.current_table_layout_request_key()?;
        let identity = storage.ivars().document.borrow().clone();
        if key.presentation_mode != MarkdownPresentationMode::InlinePreview
            || self
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .is_none_or(|cache| {
                    cache.tables.is_empty()
                        || cache.request_key != key
                        || !std::rc::Weak::ptr_eq(&cache.document_identity, &identity)
                })
        {
            // 普通正文或source模式不引入表格连续性权限。
            return None;
        }
        let is_current =
            || self.table_input_intent_is_current(&storage, &identity, key, input_epoch);
        let anchor =
            native_markdown_table_intent_step(&is_current, || self.capture_stable_table_anchor())
                .ok()?
                .ok()?;
        let after_key = native_markdown_table_intent_step(&is_current, || {
            self.current_table_layout_request_key()
        })
        .ok()??;
        if after_key != key {
            // capture期间布局改变了viewport，不能把旧key与新paragraph拼成一个收据。
            return None;
        }
        Some(NativeMarkdownTableMutationAnchor {
            identity,
            generation: key.document_generation,
            width: key.text_view_width,
            mode: key.presentation_mode,
            input_epoch,
            paragraph: anchor.source,
            viewport: key.native_viewport_start..key.native_viewport_end,
            screen_y: anchor.screen_y,
        })
    }

    /// view的storage backlink由构造固定；native步骤后只读Rust状态封闭重入，不再调用getter。
    pub(super) fn table_input_intent_is_current(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        identity: &WeakMarkdownDocument,
        key: NativeMarkdownTableLayoutRequestKey,
        input_epoch: u64,
    ) -> bool {
        if self.ivars().textkit_graph_teardown.get()
            || self.ivars().table_input_intent_epoch.get() != input_epoch
            || !storage.ivars().accepts_edits.get()
            || storage.presentation_mode() != key.presentation_mode
            || storage.ivars().table_projection_width_key.get() != key.text_view_width
        {
            // resize入口在super前推进纯width权限；关闭、新输入和mode变化同样撤销旧动作。
            return false;
        }
        #[cfg(feature = "native-table-row-append")]
        if !self.table_append_commit_is_current(input_epoch) {
            return false; // 派生 Ready 或字段激活不能重领已失效的原提交选择票。
        }
        let current = storage.ivars().document.borrow().clone();
        if !std::rc::Weak::ptr_eq(&current, identity) {
            // 同generation的新note不共享旧source身份。
            return false;
        }
        let Some(document) = current.upgrade() else {
            // Weak相同但owner已释放时没有继续步骤的权限。
            return false;
        };
        let document = document.borrow();
        document.generation() == key.document_generation
            && document.utf16_len() == key.document_length
    }

    /// 成功receipt后只发布当前动作的已映射权限；错误不伪造旧source坐标。
    pub(super) fn arm_table_mutation_anchor(
        &self,
        mut anchor: Option<NativeMarkdownTableMutationAnchor>,
        range: &std::ops::Range<usize>,
        delta: isize,
    ) {
        let generation = self
            .ivars()
            .text_storage
            .load()
            .and_then(|s| s.document())
            .map(|d| d.borrow().generation());
        if let Some(value) = anchor.as_mut()
            && (value.input_epoch != self.ivars().table_input_intent_epoch.get()
                || generation
                    .and_then(|generation| value.advance(range, delta, generation))
                    .is_none())
        {
            // 多代重入、删除锚点或坐标溢出都撤销连续性，正文成功仍保持真实状态。
            self.table_edit_diagnostic("anchor_mapping_failed");
            anchor = None;
        }
        *self.ivars().table_mutation_anchor.borrow_mut() = anchor;
        self.table_edit_diagnostic("anchor_armed");
    }

    /// 每个AX/程序/真实事件入口返回前消费或撤销，锚点绝不留给后来的用户输入。
    pub(super) fn finish_table_mutation_continuity(&self) {
        if self.ivars().table_mutation_anchor.borrow().is_none() {
            return;
        }
        self.table_edit_diagnostic("continuity_finish_before");
        if self.ivars().textkit_input_depth.get() == 0
            && !self.ivars().textkit_graph_teardown.get()
            && self
                .textContentStorage()
                .is_some_and(|content| !content.hasEditingTransaction())
        {
            // 外层field动作不等于TextKit事务；只有公开许可成立才同步运行原有executor一次。
            self.schedule_visible_table_layout_update();
            self.run_scheduled_table_layout_update();
        }
        let remaining = self.ivars().table_mutation_anchor.borrow_mut().take();
        if remaining.is_some() {
            // 当前边界仍在编辑/重建不能延后拉回用户；本次连续性明确失败。
            self.table_edit_diagnostic("continuity_not_consumed_failed");
        }
        self.table_edit_diagnostic("continuity_finish_after");
    }

    /// 新物理意图只撤销旧权限，不把native布局引起的bounds变化误判成手动滚动。
    pub(super) fn cancel_table_continuity_for_new_input(&self) {
        if self.ivars().textkit_graph_teardown.get() {
            // 迟到激活入口不能把accepted空壳的零epoch重新变成活动业务状态。
            return;
        }
        self.cancel_pending_find_reveal();
        self.cancel_pending_mode_viewport();
        #[cfg(feature = "native-table-row-append")]
        self.cancel_pending_table_append();
        // 本地尚未arm的capture也绑定此epoch，防validation重入新事件后旧局部值重新发布。
        self.ivars().table_input_intent_epoch.set(
            self.ivars()
                .table_input_intent_epoch
                .get()
                .checked_add(1)
                .expect("input intent epoch overflow"),
        );
        let old = self.ivars().table_mutation_anchor.borrow_mut().take();
        let pointer = self
            .ivars()
            .active_table_cell
            .borrow_mut()
            .as_mut()
            .and_then(|state| state.pointer.take());
        if let Some(pointer) = pointer {
            pointer.cancel();
        }
        self.ivars().pending_table_cell_activation.set(None);
        if old.is_some() {
            self.table_edit_diagnostic("new_input_cancelled_old_anchor");
        }
    }

    /// 诊断feature内固定最多256个阶段；普通产品不格式化、不写逐行stderr。
    pub(super) fn table_edit_diagnostic(&self, stage: &'static str) {
        #[cfg(feature = "native-table-edit-diagnostic")]
        {
            static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < 256 {
                let key = self.current_table_layout_request_key();
                eprintln!(
                    "table_edit_stage n={} stage={} key={:?} anchor_pending={}",
                    n,
                    stage,
                    key,
                    self.ivars().table_mutation_anchor.borrow().is_some()
                );
            } else if n == 256 {
                // 截断后不能把缺阶段当完整诊断，固定一次明确溢出标量。
                eprintln!("table_edit_diagnostic_overflow=true diagnostic_complete=false");
            }
        }
        #[cfg(not(feature = "native-table-edit-diagnostic"))]
        let _ = stage;
    }

    /// 最多两次有界准备；第二次只为覆盖高度变化后的实际viewport，不无限追逐估计值。
    fn prepare_stable_table_batch(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        identity: &WeakMarkdownDocument,
        original: NativeMarkdownTableLayoutRequestKey,
        initial_viewport: std::ops::Range<usize>,
        previous: Option<&NativeMarkdownVisibleTableLayoutCache>,
        target: NativeMarkdownTablePreparationTarget<'_>,
    ) -> Result<NativeMarkdownStableTableBatch, NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let (mutation, reveal, mode) = match target {
            NativeMarkdownTablePreparationTarget::Viewport => (None, None, None),
            NativeMarkdownTablePreparationTarget::Mutation(anchor) => (Some(anchor), None, None),
            NativeMarkdownTablePreparationTarget::Find(target) => (None, Some(target), None),
            NativeMarkdownTablePreparationTarget::Mode(target) => (None, None, Some(target)),
        };
        // slot消费只转移局部权限，不能丢掉其epoch；普通重建也绑定本次入口意图。
        let input_epoch = mutation.as_ref().map_or_else(
            || self.ivars().table_input_intent_epoch.get(),
            |anchor| anchor.input_epoch,
        );
        let is_current = || {
            self.table_input_intent_is_current(storage, identity, original, input_epoch)
                && reveal.is_none_or(|target| target.is_current(self))
                && mode.is_none_or(|target| target.is_current(self))
        };
        if original.presentation_mode != MarkdownPresentationMode::InlinePreview
            || original.document_length == 0
        {
            // Source没有表格overlay，但Find仍须布局有限目标并核对真实segment。
            if let Some(reveal) = reveal {
                self.prepare_find_reveal_without_tables(reveal, &is_current)?;
            }
            if let Some(mode) = mode {
                self.prepare_mode_viewport_without_tables(mode, &is_current).inspect_err(|_error| {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    native_markdown_trace! {
                        // 只附加原Source准备返回的Failure，不重新执行ensure或读取失败后的新视口。
                        eprintln!("native_mode_viewport stage=source_prepare_failure error={_error:?} source={}..{} width={}", initial_viewport.start, initial_viewport.end, original.text_view_width);
                    }
                })?;
            }
            let key = native_markdown_table_intent_step(&is_current, || {
                self.current_table_layout_request_key()
            })?
            .ok_or(Failure::MissingGeometry)?;
            return Ok((initial_viewport.clone(), Vec::new(), key, initial_viewport));
        }
        native_markdown_table_intent_step(&is_current, || ())?;
        let (mut working, mut anchor) = if let Some(mode) = mode {
            // 首批沿切模式前的源window工作，不让旧pixel位置在新投影中变成另一组源行。
            let anchor = mode.request.anchor.borrow();
            let anchor = anchor.as_ref().ok_or(Failure::AnchorUnavailable)?;
            (
                native_markdown_bounded_table_layout_range(&anchor.source_window, &anchor.probe)
                    .ok_or(Failure::CoverageOutsideBudget)?,
                None,
            )
        } else if let Some(reveal) = reveal {
            (
                native_markdown_bounded_table_layout_range(&initial_viewport, &reveal.probe())
                    .ok_or(Failure::CoverageOutsideBudget)?,
                None,
            )
        } else if let Some(mutation) = mutation {
            // 此source窗口在mutation前捕获并按receipt映射；不能用已跳走的viewport替换它。
            (
                mutation.viewport,
                Some(NativeMarkdownTableViewportAnchor {
                    source: mutation.paragraph,
                    screen_y: mutation.screen_y,
                }),
            )
        } else {
            (initial_viewport, None)
        };
        for pass in 0..2 {
            let document_current = native_markdown_table_intent_step(&is_current, || {
                self.table_layout_document_is_current(storage, identity, original)
            })?;
            let content =
                native_markdown_table_intent_step(&is_current, || self.textContentStorage())?;
            let editing = match content {
                Some(content) => native_markdown_table_intent_step(&is_current, || {
                    content.hasEditingTransaction()
                })?,
                None => true, // 断开图没有枚举或恢复权限。
            };
            if !document_current || editing {
                // 不跨实际编辑事务发布属性或查询layout，关闭/换文档也没有重试权限。
                return Err(Failure::MissingGeometry);
            }
            let snapshot = native_markdown_table_intent_step(&is_current, || {
                storage.table_presentation_snapshot(working.clone())
            })?
            .ok_or(Failure::MissingGeometry)?;
            let requires_tables =
                native_markdown_table_window_requires_layout(snapshot.tables(), &working)
                    .ok_or(Failure::InvalidSourceAssociation)?;
            #[cfg(feature = "native-table-edit-diagnostic")]
            eprintln!(
                "table_candidate_scope pass={} working={:?} semantic_window={:?} prototypes={} requires_tables={}",
                pass + 1,
                working,
                snapshot.utf16_window,
                snapshot.tables().len(),
                requires_tables
            );
            if !requires_tables {
                // core 的有限 overscan 可包含完全离屏的前表；它们不应让当前 quote/code 区间
                // 进入 table builder 并把合法空结果误报为预算失败。这里只证明 working 无表，
                // 不证明 SourceFallback 完整预览；Find/Mode 仍走原定位、覆盖与同票据认证。
                native_markdown_table_intent_step(&is_current, || {
                    storage.install_table_row_heights(
                        identity,
                        original.document_generation,
                        original.text_view_width,
                        Vec::new(),
                    )
                })?
                .ok_or(Failure::CoverageOutsideBudget)?;
                if let Some(reveal) = reveal {
                    // 移除离屏旧高度也可移动尾段；普通区间不能跳过目标定位。
                    self.prepare_find_reveal_without_tables(reveal, &is_current)?;
                }
                if let Some(mode) = mode {
                    self.prepare_mode_viewport_without_tables(mode, &is_current)?;
                    let visible =
                        native_markdown_table_intent_step(&is_current, || self.visibleRect())?;
                    let current = native_markdown_table_intent_step(&is_current, || {
                        self.read_visible_utf16_window(visible, original.document_length)
                    })?;
                    if current.is_empty() {
                        return Err(Failure::MissingGeometry);
                    }
                    if current.start < snapshot.utf16_window.start
                        || current.end > snapshot.utf16_window.end
                    {
                        if pass != 0 {
                            return Err(Failure::CoverageOutsideBudget);
                        }
                        working = native_markdown_bounded_table_layout_range(&working, &current)
                            .ok_or(Failure::CoverageOutsideBudget)?;
                        continue; // 唯一第二轮覆盖实际新viewport，不排新任务或重试loop。
                    }
                }
                let key = native_markdown_table_intent_step(&is_current, || {
                    self.current_table_layout_request_key()
                })?
                .ok_or(Failure::MissingGeometry)?;
                return Ok((snapshot.utf16_window, Vec::new(), key, working));
            }
            if native_markdown_table_preparation_budget(snapshot.tables(), &working).is_none() {
                // 第二次扩张与第一次使用相同先验总预算，不先分配再截断。
                return Err(Failure::CoverageOutsideBudget);
            }
            let visible = native_markdown_table_intent_step(&is_current, || self.visibleRect())?;
            let mut tables = native_markdown_table_intent_step(&is_current, || {
                self.build_visible_table_layouts(storage, &snapshot, &working, visible, previous)
            })?;
            if tables.is_empty() {
                return Err(Failure::CoverageOutsideBudget);
            }
            if anchor.is_none() && reveal.is_none() && mode.is_none() {
                // 只在第一次改变高度前记录原屏幕source anchor；后续不能以新位置覆盖目标。
                anchor = Some(native_markdown_table_intent_step(&is_current, || {
                    self.capture_stable_table_anchor()
                })??);
            }
            let anchor = anchor.as_ref();
            let target_source = reveal
                .map(|target| target.probe())
                .or_else(|| mode.map(|target| target.probe()))
                .or_else(|| anchor.map(|value| value.source.clone()))
                .ok_or(Failure::AnchorUnavailable)?;
            let rows_start = tables
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| row.source_utf16_range.start)
                .min()
                .ok_or(Failure::MissingGeometry)?;
            let rows_end = tables
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| row.source_utf16_range.end)
                .max()
                .ok_or(Failure::MissingGeometry)?;
            let mut ensure =
                native_markdown_bounded_table_layout_range(&(rows_start..rows_end), &target_source)
                    .ok_or(Failure::CoverageOutsideBudget)?;
            for table in &tables {
                if table
                    .rows
                    .first()
                    .is_some_and(|row| row.row_index == 0 && row.is_header)
                {
                    // header-only也须在这唯一一次ensure中纳入真实delimiter；中段不追读离屏表头。
                    ensure = native_markdown_bounded_table_layout_range(
                        &ensure,
                        &table.delimiter_utf16_range,
                    )
                    .ok_or(Failure::CoverageOutsideBudget)?;
                }
            }
            let mut heights = Vec::new();
            for table in &tables {
                // 第二批working是第一批与新viewport的连续union，旧测量行不会被先恢复literal。
                // 此时仍是fresh builder的Native测高；稍后header visual band绝不能反馈到这里累计+1。
                heights.push(NativeMarkdownTableRowHeight {
                    source_utf16_range: table.delimiter_utf16_range.clone(),
                    height: NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT,
                });
                heights.extend(table.rows.iter().map(|row| NativeMarkdownTableRowHeight {
                    source_utf16_range: row.source_utf16_range.clone(),
                    height: row.frame.size.height,
                }));
            }
            heights.sort_unstable_by_key(|row| row.source_utf16_range.start);
            native_markdown_table_intent_step(&is_current, || {
                storage.install_table_row_heights(
                    identity,
                    original.document_generation,
                    original.text_view_width,
                    heights,
                )
            })?
            .ok_or(Failure::CoverageOutsideBudget)?;
            // 本准备阶段唯一的连续ensure；逐行lookup均无ensure。
            self.ensure_stable_table_source_range(&ensure, &is_current)?;
            let manager =
                native_markdown_table_intent_step(&is_current, || self.textLayoutManager())?
                    .ok_or(Failure::MissingGeometry)?;
            let viewport = native_markdown_table_intent_step(&is_current, || {
                manager.textViewportLayoutController()
            })?;
            #[cfg(feature = "native-table-edit-diagnostic")]
            let phase_diagnostic = {
                use std::sync::atomic::{AtomicBool, Ordering};
                static USED: AtomicBool = AtomicBool::new(false);
                let eligible = pass == 0
                    && std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
                    && mode.is_some_and(|mode| {
                        let stamp = mode.request.stamp.get();
                        stamp.before == MarkdownPresentationMode::Source
                            && stamp.after == MarkdownPresentationMode::InlinePreview
                    });
                if eligible
                    && USED
                        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                {
                    self.trace_table_preparation_phase(
                        "after_ensure",
                        storage,
                        &manager,
                        &tables,
                        None,
                        &is_current,
                    )
                } else {
                    None // 固定一次批次预算，不因准备失败或新mode动作重置观察权限。
                }
            };
            native_markdown_table_intent_step(&is_current, || viewport.layoutViewport())?;
            if mode.is_some() {
                // 可见glyph属于真实行中的CTFrame，先认证/更新有限候选矩阵，不能先使用零Y或行占位框。
                self.resolve_stable_table_matrix(
                    storage,
                    &snapshot,
                    &mut tables,
                    &working,
                    &is_current,
                )?;
            }
            let positioned = if let Some(reveal) = reveal {
                // 同一批行高已发布且唯一连续ensure已完成；目标segment取代当前顶部旧锚点。
                self.position_find_reveal(reveal)
            } else if let Some(mode) = mode {
                self.position_mode_viewport(mode, Some(&tables))
            } else if let Some(anchor) = anchor {
                self.restore_stable_table_anchor(anchor, &is_current)
            } else {
                Ok(()) // 无原语义目标时没有位置恢复动作，仍继续认证同一批Native矩阵。
            };
            #[cfg(feature = "native-table-edit-diagnostic")]
            if let Some(before) = phase_diagnostic.as_ref() {
                drop(self.trace_table_preparation_phase(
                    if positioned.is_ok() {
                        "after_position"
                    } else {
                        "after_position_failed"
                    },
                    storage,
                    &manager,
                    &tables,
                    Some(before),
                    &is_current,
                )); // 同一次原请求的第二个真实边界，不重试或为观测追加layout。
            }
            positioned?; // 失败诊断不消费或替换原错误，也不能把已失败定位送入矩阵发布。
            let visible = native_markdown_table_intent_step(&is_current, || self.visibleRect())?;
            let current = native_markdown_table_intent_step(&is_current, || {
                self.read_visible_utf16_window(visible, original.document_length)
            })?;
            if current.is_empty() {
                return Err(Failure::MissingGeometry);
            }
            native_markdown_table_intent_step(&is_current, || {
                self.resolve_stable_table_matrix(
                    storage,
                    &snapshot,
                    &mut tables,
                    &current,
                    &is_current,
                )
            })??; // 剩余全部可见行仍过原两遍身份、几何、连续性门，任何可见缺口照常失败。
            // 原矩阵getter也可能兑现TextKit延迟几何；用后读的实际clip核同批位置，变化交原第二批。
            let actual_visible =
                native_markdown_table_intent_step(&is_current, || self.visibleRect())?;
            let mode_stable = if let Some(mode) = mode {
                let probe = mode.probe();
                let frame = match native_markdown_table_scalar_frame(&tables, &probe)? {
                    Some(frame) => frame,
                    None => self.source_probe_segment(&probe, &is_current)?, // 目标是同视口普通正文时仍用其原Native字形。
                };
                let actual_y = frame.origin.y - actual_visible.origin.y;
                let expected_y = mode
                    .request
                    .applied_screen_y
                    .get()
                    .ok_or(Failure::AnchorUnavailable)?;
                mode_probe_is_visible(frame, actual_visible)
                    && (actual_y - expected_y).abs() <= 0.5
                    && (actual_visible.origin.x - visible.origin.x).abs() <= 0.01
                    && (actual_visible.origin.y - visible.origin.y).abs() <= 0.01
                    && (actual_visible.size.width - visible.size.width).abs() <= 0.01
                    && (actual_visible.size.height - visible.size.height).abs() <= 0.01
            } else {
                true
            };
            let covered = mode_stable
                && current.start >= snapshot.utf16_window.start
                && current.end <= snapshot.utf16_window.end
                && snapshot
                    .tables()
                    .iter()
                    .filter(|table| {
                        table.utf16_range.start < current.end
                            && table.utf16_range.end > current.start
                    })
                    .all(|prototype| {
                        // 以同一个原Native范围逐原型核全可见行/格，裁离屏绝不能掩盖可见缺失。
                        let first = prototype
                            .rows
                            .partition_point(|row| row.utf16_range.end <= current.start);
                        let end = prototype
                            .rows
                            .partition_point(|row| row.utf16_range.start < current.end);
                        let Some(table) = tables
                            .iter()
                            .find(|table| table.table_utf16_start == prototype.utf16_range.start)
                        else {
                            return first == end;
                        };
                        native_markdown_table_rows_cover(&table.rows, first, end)
                            && prototype.rows[first..end]
                                .iter()
                                .enumerate()
                                .all(|(offset, row)| {
                                    // row完整还不够；该行每个语义cell也必须有本批投影。
                                    table
                                        .cells
                                        .iter()
                                        .filter(|cell| cell.coordinate.row_index == first + offset)
                                        .count()
                                        == row.cell_index_range.len()
                                })
                    });
            #[cfg(feature = "native-table-edit-diagnostic")]
            eprintln!(
                "table_candidate_attempt pass={} ensure={:?} prepared_window={:?} current_window={:?} covered={} anchor={} screen_y={}",
                pass + 1,
                ensure,
                working,
                current,
                covered,
                target_source.start,
                anchor.map_or(0.0, |value| value.screen_y)
            );
            if covered {
                // mode_stable已用本批同源CTFrame认证；最终原executor将在发布cache后再独立复核。
                if let Some(anchor) = anchor {
                    native_markdown_table_intent_step(&is_current, || {
                        self.verify_stable_table_anchor(anchor)
                    })??;
                }
                // Find的目标已由position认证；后面的最终身份检查仍覆盖所有Native读取。
                let final_key = native_markdown_table_intent_step(&is_current, || {
                    self.current_table_layout_request_key()
                })?
                .ok_or(Failure::MissingGeometry)?;
                let document_current = native_markdown_table_intent_step(&is_current, || {
                    self.table_layout_document_is_current(storage, identity, original)
                })?;
                if !document_current
                    || final_key.text_view_width != original.text_view_width
                    || final_key.native_viewport_start != current.start
                    || final_key.native_viewport_end != current.end
                    || !is_current()
                {
                    return Err(Failure::UnstableFrames);
                }
                return Ok((snapshot.utf16_window, tables, final_key, current));
            }
            if pass == 1 {
                // 两次准备后仍不覆盖必须保留明确失败，不排队自旋等待系统“稳定”。
                return Err(Failure::PreparationPassLimit);
            }
            working = native_markdown_bounded_table_layout_range(&working, &current)
                .ok_or(Failure::CoverageOutsideBudget)?;
        }
        Err(Failure::PreparationPassLimit)
    }

    /// 只读公开fragment getter；不会建立逐行布局岛。
    fn stable_table_fragment(
        &self,
        offset: usize,
    ) -> Option<Retained<objc2_app_kit::NSTextLayoutFragment>> {
        let manager = self.textLayoutManager()?;
        let content = manager.textContentManager()?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let start = provider.documentRange().location();
        let location =
            content.locationFromLocation_withOffset(&start, isize::try_from(offset).ok()?)?;
        manager.textLayoutFragmentForLocation(&location)
    }

    pub(super) fn stable_fragment_source(
        &self,
        fragment: &objc2_app_kit::NSTextLayoutFragment,
    ) -> Option<std::ops::Range<usize>> {
        let content = self.textLayoutManager()?.textContentManager()?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let start = provider.documentRange().location();
        let range = fragment.rangeInElement();
        Some(
            usize::try_from(content.offsetFromLocation_toLocation(&start, &range.location()))
                .ok()?
                ..usize::try_from(
                    content.offsetFromLocation_toLocation(&start, &range.endLocation()),
                )
                .ok()?,
        )
    }

    fn capture_stable_table_anchor(
        &self,
    ) -> Result<NativeMarkdownTableViewportAnchor, NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let manager = self.textLayoutManager().ok_or(Failure::AnchorUnavailable)?;
        let visible = self.visibleRect();
        let origin = self.textContainerOrigin();
        let mut fragment = manager
            .textLayoutFragmentForPosition(NSPoint::new(
                5.0,
                (visible.origin.y - origin.y).max(0.0),
            ))
            .ok_or(Failure::AnchorUnavailable)?;
        if fragment.layoutFragmentFrame().origin.y + origin.y < visible.origin.y - 0.01 {
            // 顶部半行的高度可能改变，选择下一条完整paragraph起点保持语义屏幕位置。
            fragment = manager
                .textLayoutFragmentForLocation(&fragment.rangeInElement().endLocation())
                .ok_or(Failure::AnchorUnavailable)?;
        }
        let source = self
            .stable_fragment_source(&fragment)
            .ok_or(Failure::AnchorUnavailable)?;
        let screen_y = fragment.layoutFragmentFrame().origin.y + origin.y - visible.origin.y;
        if fragment.state() != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
            || !screen_y.is_finite()
            || screen_y < -0.01
            || screen_y > visible.size.height
            || source.is_empty()
        {
            return Err(Failure::AnchorUnavailable);
        }
        Ok(NativeMarkdownTableViewportAnchor { source, screen_y })
    }

    pub(super) fn ensure_stable_table_source_range(
        &self,
        range: &std::ops::Range<usize>,
        is_current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let manager = native_markdown_table_intent_step(is_current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let content =
            native_markdown_table_intent_step(is_current, || manager.textContentManager())?
                .ok_or(Failure::MissingGeometry)?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range =
            native_markdown_table_intent_step(is_current, || provider.documentRange())?;
        let document_start =
            native_markdown_table_intent_step(is_current, || document_range.location())?;
        let start_offset =
            isize::try_from(range.start).map_err(|_| Failure::InvalidSourceAssociation)?;
        let end_offset =
            isize::try_from(range.end).map_err(|_| Failure::InvalidSourceAssociation)?;
        let start = native_markdown_table_intent_step(is_current, || {
            content.locationFromLocation_withOffset(&document_start, start_offset)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let end = native_markdown_table_intent_step(is_current, || {
            content.locationFromLocation_withOffset(&document_start, end_offset)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        let allocated = native_markdown_table_intent_step(is_current, NSTextRange::alloc)?;
        let native = native_markdown_table_intent_step(is_current, || {
            NSTextRange::initWithLocation_endLocation(allocated, &start, Some(&end))
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        native_markdown_table_intent_step(is_current, || manager.ensureLayoutForRange(&native))?;
        Ok(())
    }

    fn restore_stable_table_anchor(
        &self,
        anchor: &NativeMarkdownTableViewportAnchor,
        is_current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let fragment = native_markdown_table_intent_step(is_current, || {
            self.stable_table_fragment(anchor.source.start)
        })?
        .ok_or(Failure::AnchorUnstable)?;
        let state = native_markdown_table_intent_step(is_current, || fragment.state())?;
        let source = native_markdown_table_intent_step(is_current, || {
            self.stable_fragment_source(&fragment)
        })?;
        if state != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
            || source.as_ref() != Some(&anchor.source)
        {
            // 相同offset若已归属另一段或只有估计frame，不能用于恢复旧语义锚点。
            return Err(Failure::AnchorUnstable);
        }
        let frame =
            native_markdown_table_intent_step(is_current, || fragment.layoutFragmentFrame())?;
        let origin = native_markdown_table_intent_step(is_current, || self.textContainerOrigin())?;
        let desired = frame.origin.y + origin.y - anchor.screen_y;
        if !desired.is_finite() || desired < -0.01 {
            return Err(Failure::AnchorUnstable);
        }
        let visible = native_markdown_table_intent_step(is_current, || self.visibleRect())?;
        if (visible.origin.y - desired).abs() > 0.01 {
            // 公开clip滚动只调整当前viewport，不改source或强行写fragment frame。
            let scroll =
                native_markdown_table_intent_step(is_current, || self.enclosingScrollView())?
                    .ok_or(Failure::AnchorUnstable)?;
            let clip = native_markdown_table_intent_step(is_current, || scroll.contentView())?;
            let clip_bounds = native_markdown_table_intent_step(is_current, || clip.bounds())?;
            native_markdown_table_intent_step(is_current, || {
                clip.scrollToPoint(NSPoint::new(clip_bounds.origin.x, desired.max(0.0)))
            })?;
            native_markdown_table_intent_step(is_current, || {
                scroll.reflectScrolledClipView(&clip)
            })?;
            let manager =
                native_markdown_table_intent_step(is_current, || self.textLayoutManager())?
                    .ok_or(Failure::MissingGeometry)?;
            let viewport = native_markdown_table_intent_step(is_current, || {
                manager.textViewportLayoutController()
            })?;
            native_markdown_table_intent_step(is_current, || viewport.layoutViewport())?;
        }
        native_markdown_table_intent_step(is_current, || self.verify_stable_table_anchor(anchor))?
    }

    fn verify_stable_table_anchor(
        &self,
        anchor: &NativeMarkdownTableViewportAnchor,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let fragment = self
            .stable_table_fragment(anchor.source.start)
            .ok_or(Failure::AnchorUnstable)?;
        let screen_y = fragment.layoutFragmentFrame().origin.y + self.textContainerOrigin().y
            - self.visibleRect().origin.y;
        if fragment.state() != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
            || self.stable_fragment_source(&fragment).as_ref() != Some(&anchor.source)
            || !screen_y.is_finite()
            || (screen_y - anchor.screen_y).abs() > 0.5
        {
            return Err(Failure::AnchorUnstable);
        }
        Ok(())
    }

    /// 比较同一准备批次两个原生边界，仅保留有限Weak与几何标量，不改变布局权限。
    #[cfg(feature = "native-table-edit-diagnostic")]
    fn trace_table_preparation_phase(
        &self,
        phase: &'static str,
        storage: &PieceBackedMarkdownTextStorage,
        manager: &objc2_app_kit::NSTextLayoutManager,
        tables: &[NativeMarkdownVisibleTableLayout],
        before: Option<&[NativeMarkdownTablePhaseDiagnosticRow]>,
        current: &impl Fn() -> bool,
    ) -> Option<Vec<NativeMarkdownTablePhaseDiagnosticRow>> {
        let result = (|| -> Result<Vec<NativeMarkdownTablePhaseDiagnosticRow>, NativeMarkdownStableTableFailure> {
            use NativeMarkdownStableTableFailure as Failure;
            let content = native_markdown_table_intent_step(current, || manager.textContentManager())?
                .ok_or(Failure::MissingGeometry)?;
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let document = native_markdown_table_intent_step(current, || provider.documentRange())?;
            let origin = native_markdown_table_intent_step(current, || document.location())?;
            let mut captured = Vec::with_capacity(32);
            for (count, row) in tables.iter().flat_map(|table| &table.rows).take(33).enumerate() {
                if count == 32 {
                    eprintln!("native_table_geometry_phase stage={phase} event=truncated limit=32 formal=false");
                    break; // 超预算不取第33行的Native值，也不把未观察范围解释成无断层。
                }
                let offset = isize::try_from(row.source_utf16_range.start)
                    .map_err(|_| Failure::InvalidSourceAssociation)?;
                let location = native_markdown_table_intent_step(current, || {
                    content.locationFromLocation_withOffset(&origin, offset)
                })?.ok_or(Failure::InvalidSourceAssociation)?;
                let fragment = native_markdown_table_intent_step(current, || {
                    manager.textLayoutFragmentForLocation(&location)
                })?.ok_or(Failure::MissingGeometry)?;
                let state = native_markdown_table_intent_step(current, || fragment.state())?;
                let source = native_markdown_table_intent_step(current, || fragment.rangeInElement())?;
                let start = native_markdown_table_intent_step(current, || source.location())?;
                let end = native_markdown_table_intent_step(current, || source.endLocation())?;
                let from = native_markdown_table_intent_step(current, || content.offsetFromLocation_toLocation(&origin, &start))?;
                let to = native_markdown_table_intent_step(current, || content.offsetFromLocation_toLocation(&origin, &end))?;
                let actual_manager = native_markdown_table_intent_step(current, || fragment.textLayoutManager())?;
                let manager_matches = actual_manager.as_deref().is_some_and(|actual| std::ptr::eq::<objc2_app_kit::NSTextLayoutManager>(actual, manager));
                let (frame, margins) = if state == objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable {
                    let frame = native_markdown_table_intent_step(current, || fragment.layoutFragmentFrame())?;
                    let top = native_markdown_table_intent_step(current, || fragment.topMargin())?;
                    let bottom = native_markdown_table_intent_step(current, || fragment.bottomMargin())?;
                    (Some(frame), Some((top, bottom))) // 只读真实state3几何；未就绪不能猜frame或强制ensure。
                } else { (None, None) };
                let original = before.and_then(|before| before.get(count));
                let original_claim = match original {
                    Some(original) => native_markdown_table_intent_step(current, || original.fragment.load())?,
                    None => None, // 首阶段尚无原Weak，不以相同地址数字伪造身份来源。
                };
                let same_original = original_claim.as_deref().map(|original| std::ptr::eq::<objc2_app_kit::NSTextLayoutFragment>(original, &*fragment));
                let metadata_matches = original.map(|original| original.row == row.row_index && original.source == row.source_utf16_range);
                let address = std::ptr::from_ref(&*fragment).addr();
                let weak = if before.is_none() {
                    Some(native_markdown_table_intent_step(current, || Weak::from_retained(&fragment))?)
                } else { None };
                drop(original_claim);
                drop(actual_manager);
                drop(end);
                drop(start);
                drop(source);
                drop(fragment);
                drop(location);
                native_markdown_table_intent_step(current, || ())?; // Native临时claims已归还，失权的标量不得继续报告为原票事实。
                eprintln!("native_table_geometry_phase stage={phase} index={count} row={} expected_source={:?} actual_source={from}..{to} ptr={address:#x} state={} frame={frame:?} margins={margins:?} manager_matches={manager_matches} same_original={same_original:?} metadata_matches={metadata_matches:?} table_epoch={} font_epoch={} semantic_epoch={:?} input_epoch={} formal=false", row.row_index, row.source_utf16_range, state.0, storage.ivars().table_geometry_epoch.get(), storage.ivars().font_run_scratch.epoch(), storage.ivars().presentation_semantic_epoch.get(), self.ivars().table_input_intent_epoch.get());
                if let Some(fragment) = weak {
                    captured.push(NativeMarkdownTablePhaseDiagnosticRow { row: row.row_index, source: row.source_utf16_range.clone(), fragment });
                }
            }
            Ok(captured)
        })();
        match result {
            Ok(rows) if current() => Some(rows),
            Ok(_) => None, // 原操作已撤销，只丢观察；下一原业务Native步骤仍按原票拒绝。
            Err(error) => {
                eprintln!(
                    "native_table_geometry_phase stage={phase} event=unavailable error={error:?} formal=false"
                );
                None // 诊断不能把失败改成业务成功，也不能重新领票或布局后补观测。
            }
        }
    }

    /// 全部候选片两遍只读；完整可见源流严格认证，可选预读仅沿真实相接框扩展。
    fn resolve_stable_table_matrix(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        snapshot: &NativeMarkdownTablePresentationSnapshot,
        tables: &mut Vec<NativeMarkdownVisibleTableLayout>,
        viewport_source: &std::ops::Range<usize>,
        is_current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        // selection可改变隐藏delimiter而不改正文generation；同一语义epoch贯穿两矩阵。
        let semantic_epoch = storage.ivars().font_run_scratch.epoch();
        let matrix_current = || {
            is_current()
                && storage.ivars().font_run_scratch.epoch() == semantic_epoch
                && snapshot.is_current(storage)
        };
        let mut matrix = Vec::new();
        for table in tables.iter() {
            for row in &table.rows {
                // 只有实际准备的header才读取delimiter；中段只读其有界Native rows。
                matrix.push(self.read_stable_table_paragraph(
                    storage,
                    &row.source_utf16_range,
                    row.frame.size.height,
                    &matrix_current,
                )?);
                if row.row_index == 0 && row.is_header {
                    if !self.table_delimiter_is_hidden(
                        storage,
                        snapshot,
                        &table.delimiter_utf16_range,
                    ) {
                        return Err(Failure::InvalidSourceAssociation); // 已显露语法不能被header band遮盖。
                    }
                    matrix.push(self.read_stable_table_paragraph(
                        storage,
                        &table.delimiter_utf16_range,
                        NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT,
                        &matrix_current,
                    )?);
                }
            }
        }
        let mut index = 0;
        for table in tables.iter() {
            for row in &table.rows {
                let second = self.read_stable_table_paragraph(
                    storage,
                    &row.source_utf16_range,
                    row.frame.size.height,
                    &matrix_current,
                )?;
                let first = matrix.get(index).ok_or(Failure::UnstableFrames)?;
                self.verify_same_table_paragraph(first, &second, &matrix_current)?;
                index += 1;
                if row.row_index == 0 && row.is_header {
                    // delimiter也须第二次读取真实fragment，不能仅用第一遍坐标或gap算术补1pt。
                    let second = self.read_stable_table_paragraph(
                        storage,
                        &table.delimiter_utf16_range,
                        NATIVE_MARKDOWN_TABLE_DELIMITER_HEIGHT,
                        &matrix_current,
                    )?;
                    let first = matrix.get(index).ok_or(Failure::UnstableFrames)?;
                    self.verify_same_table_paragraph(first, &second, &matrix_current)?;
                    index += 1;
                }
            }
        }
        // 两次真实读取已证明每个候选片身份/几何稳定。可见源行全部必选；离屏预读
        // 只有沿真实框连续连接到这些必选行才可延伸，不能把旧坐标的离屏岛混入矩阵。
        let mut selected_native = Vec::with_capacity(tables.len());
        let mut matrix_start = 0;
        for table in tables.iter_mut() {
            let header = table
                .rows
                .first()
                .is_some_and(|row| row.row_index == 0 && row.is_header);
            let native_index = |row: usize| matrix_start + row + usize::from(header && row > 0);
            let matrix_end = native_index(table.rows.len());
            let intersects = |row: &NativeMarkdownVisibleTableRowLayout| {
                row.source_utf16_range.start < viewport_source.end
                    && viewport_source.start < row.source_utf16_range.end
            };
            let first = table.rows.iter().position(&intersects);
            let last = table.rows.iter().rposition(&intersects);
            if let (Some(mut first), Some(last)) = (first, last) {
                let mut end = last + 1;
                let connects = |previous: usize, next: usize| {
                    let before = &matrix[native_index(previous)];
                    let after = &matrix[native_index(next)];
                    let mut bottom = before.frame.origin.y + before.frame.size.height;
                    if header && previous == 0 {
                        let delimiter = &matrix[native_index(previous) + 1];
                        if (delimiter.frame.origin.y - bottom).abs() > 0.01 {
                            return false; // header不能跨过未相接的真实delimiter补造band。
                        }
                        bottom = delimiter.frame.origin.y + delimiter.frame.size.height;
                    }
                    table.rows[previous].row_index.checked_add(1)
                        == Some(table.rows[next].row_index)
                        && (after.frame.origin.y - bottom).abs() <= 0.01
                }; // 仅决定可选的离屏延伸；必选范围内任何缺口仍由原完整门判失败。
                while first > 0 && connects(first - 1, first) {
                    first -= 1; // 只沿源顺序向前扩，不跳过中间旧片寻找另一岛。
                }
                while end < table.rows.len() && connects(end - 1, end) {
                    end += 1; // 保留实际相接的EOF预读行，不因恰在clip外就丢失完整末表。
                }
                selected_native.push(native_index(first)..native_index(end));
                table.rows.truncate(end);
                table.rows.drain(..first); // 不修改任何Native frame或canonical源范围。
                table.cells.retain(|cell| {
                    table
                        .rows
                        .binary_search_by_key(&cell.coordinate.row_index, |row| row.row_index)
                        .is_ok()
                }); // 命中格和取得同源权限的行同步，不能留下离屏旧框的编辑目标。
            } else {
                table.rows.clear();
                table.cells.clear(); // 不相交的表格没有本批绘制或命中权限；原语义目录不动。
            }
            matrix_start = matrix_end;
        }
        tables.retain(|table| !table.rows.is_empty());
        matrix = matrix
            .into_iter()
            .enumerate()
            .filter(|(index, _)| selected_native.iter().any(|range| range.contains(index)))
            .map(|(_, row)| row)
            .collect();
        native_markdown_table_intent_step(&matrix_current, || ())?; // Weak临时元数据归还后仍核同票。
        if !native_markdown_table_matrix_is_ordered(&matrix) {
            #[cfg(feature = "native-table-edit-diagnostic")]
            {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static REPORTED: AtomicUsize = AtomicUsize::new(0);
                if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
                    && REPORTED.fetch_add(1, Ordering::Relaxed) < 4
                {
                    for (index, row) in matrix.iter().take(32).enumerate() {
                        // 仅报告本次已捕获的有限矩阵；不为失败追读Native或复签几何权限。
                        eprintln!(
                            "native_table_matrix_failure index={index} source={:?} fragment_source={:?} frame={:?} formal=false",
                            row.source, row.fragment_source, row.frame
                        );
                    }
                }
            }
            return Err(Failure::NonMonotonicFrames);
        }
        let mut index = 0;
        for table in tables {
            let has_first_body = table
                .rows
                .get(1)
                .is_some_and(|row| row.row_index == 1 && !row.is_header);
            for row in &mut table.rows {
                let resolved = matrix.get(index).ok_or(Failure::UnstableFrames)?;
                let is_header = row.row_index == 0 && row.is_header;
                if is_header {
                    let delimiter = matrix.get(index + 1).ok_or(Failure::UnstableFrames)?;
                    let body = if has_first_body {
                        Some(matrix.get(index + 2).ok_or(Failure::UnstableFrames)?) // 同表row1才是首body。
                    } else {
                        None // header-only不借用下一张表或普通段落作为body。
                    };
                    let hidden = self.table_delimiter_is_hidden(
                        storage,
                        snapshot,
                        &table.delimiter_utf16_range,
                    );
                    let band = native_markdown_table_header_band(resolved, delimiter, body, hidden)
                        .ok_or(Failure::InvalidSourceAssociation)?;
                    row.frame.origin.y = band.origin_y;
                    row.frame.size.height = band.height;
                } else {
                    // 正文行严格使用原Native frame，不平移整表或累计delta。
                    row.frame.origin.y = resolved.frame.origin.y;
                    row.frame.size.height = resolved.frame.size.height;
                }
                let (text_y, text_height) =
                    native_markdown_table_native_content_y_height(resolved.frame)
                        .ok_or(Failure::MissingGeometry)?;
                for cell in table
                    .cells
                    .iter_mut()
                    .filter(|cell| cell.coordinate.row_index == row.row_index)
                {
                    cell.frame.origin.y = row.frame.origin.y;
                    cell.frame.size.height = row.frame.size.height;
                    // Native内容高不含delimiter；x/width继续完整列内框，不取tight glyph宽。
                    cell.text_frame.origin.y = text_y;
                    cell.text_frame.size.height = text_height;
                }
                #[cfg(feature = "native-table-edit-diagnostic")]
                eprintln!(
                    "table_candidate_row row={} source={:?} y={} h={}",
                    row.row_index,
                    row.source_utf16_range,
                    row.frame.origin.y,
                    row.frame.size.height
                );
                index += if is_header { 2 } else { 1 }; // delimiter只参与Native矩阵，不生成可编辑display row。
            }
            if !native_markdown_table_rows_have_contiguous_frames(&table.rows) {
                // 单调矩阵仍可能包含旧布局的大空洞，完整table不能跳过这些屏幕区间。
                return Err(Failure::NonMonotonicFrames);
            }
            let first = table.rows.first().ok_or(Failure::MissingGeometry)?;
            let last = table.rows.last().ok_or(Failure::MissingGeometry)?;
            table.frame.origin.y = first.frame.origin.y;
            table.frame.size.height =
                last.frame.origin.y + last.frame.size.height - first.frame.origin.y;
        }
        native_markdown_table_intent_step(&matrix_current, || ())
    }

    /// delimiter 与本批表格读取同一原始目录；Native 属性换页后也不改用后继窗口。
    fn table_delimiter_is_hidden(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        snapshot: &NativeMarkdownTablePresentationSnapshot,
        range: &std::ops::Range<usize>,
    ) -> bool {
        if !snapshot.is_current(storage) {
            return false; // 原源、选择或模式失权时，不以当前缓存给旧 band 补签隐藏权限。
        }
        snapshot.semantics.as_ref().is_some_and(|semantics| {
            native_markdown_table_delimiter_semantics_are_hidden(
                semantics,
                snapshot.document_generation,
                range,
            )
        })
    }

    /// 两次公开读取之间须仍是同一活Native fragment；Weak load也受同意图门禁。
    fn verify_same_table_paragraph(
        &self,
        first: &NativeMarkdownResolvedTableRow,
        second: &NativeMarkdownResolvedTableRow,
        is_current: &impl Fn() -> bool,
    ) -> Result<(), NativeMarkdownStableTableFailure> {
        let first_claim = native_markdown_table_intent_step(is_current, || {
            first.fragment.as_ref().and_then(Weak::load)
        })?;
        let second_claim = native_markdown_table_intent_step(is_current, || {
            second.fragment.as_ref().and_then(Weak::load)
        })?;
        if !first_claim
            .zip(second_claim)
            .is_some_and(|(first, second)| std::ptr::eq(&*first, &*second))
            || first.fragment_source != second.fragment_source
            || (first.frame.origin.x - second.frame.origin.x).abs() > 0.01
            || (first.frame.origin.y - second.frame.origin.y).abs() > 0.01
            || (first.frame.size.width - second.frame.size.width).abs() > 0.01
            || (first.frame.size.height - second.frame.size.height).abs() > 0.01
        {
            return Err(NativeMarkdownStableTableFailure::UnstableFrames); // 丢弃混合矩阵，不重试等“稳定”。
        }
        native_markdown_table_intent_step(is_current, || ())
    }

    /// delimiter与可见行共用完整paragraph/source/尾换行认证，不伪造display row索引。
    fn read_stable_table_paragraph(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        source: &std::ops::Range<usize>,
        minimum_height: f64,
        is_current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownResolvedTableRow, NativeMarkdownStableTableFailure> {
        use NativeMarkdownStableTableFailure as Failure;
        let fragment = native_markdown_table_intent_step(is_current, || {
            self.stable_table_fragment(source.start)
        })?
        .ok_or(Failure::MissingGeometry)?;
        if native_markdown_table_intent_step(is_current, || fragment.state())?
            != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
        {
            return Err(Failure::MissingGeometry);
        }
        let native_source = native_markdown_table_intent_step(is_current, || {
            self.stable_fragment_source(&fragment)
        })?
        .ok_or(Failure::InvalidSourceAssociation)?;
        if native_source.start != source.start
            || native_source.end < source.end
            || native_source.end - source.end > 2
        {
            return Err(Failure::InvalidSourceAssociation);
        }
        // SAFETY: SDK weak textElement getter可能nil，Option<Retained>只在这次完整性检查中临时持有它。
        let element: Option<Retained<objc2_app_kit::NSTextElement>> =
            native_markdown_table_intent_step(is_current, || unsafe {
                msg_send![&*fragment, textElement]
            })?;
        let element = element.ok_or(Failure::InvalidSourceAssociation)?;
        let manager = native_markdown_table_intent_step(is_current, || self.textLayoutManager())?
            .ok_or(Failure::MissingGeometry)?;
        let queue = native_markdown_table_intent_step(is_current, || manager.layoutQueue())?;
        let actual_manager =
            native_markdown_table_intent_step(is_current, || fragment.textLayoutManager())?;
        if queue.is_some()
            || !actual_manager.is_some_and(|actual| std::ptr::eq(&*actual, &*manager))
        {
            // 候选只接受同步同一manager的矩阵，异步队列不能提供此主线程快照证明。
            return Err(Failure::InvalidSourceAssociation);
        }
        let content =
            native_markdown_table_intent_step(is_current, || manager.textContentManager())?
                .ok_or(Failure::MissingGeometry)?;
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range =
            native_markdown_table_intent_step(is_current, || provider.documentRange())?;
        let document_start =
            native_markdown_table_intent_step(is_current, || document_range.location())?;
        let element_range =
            native_markdown_table_intent_step(is_current, || element.elementRange())?
                .ok_or(Failure::InvalidSourceAssociation)?;
        let element_location =
            native_markdown_table_intent_step(is_current, || element_range.location())?;
        let element_end_location =
            native_markdown_table_intent_step(is_current, || element_range.endLocation())?;
        let element_start = usize::try_from(native_markdown_table_intent_step(is_current, || {
            content.offsetFromLocation_toLocation(&document_start, &element_location)
        })?)
        .map_err(|_| Failure::InvalidSourceAssociation)?;
        let element_end = usize::try_from(native_markdown_table_intent_step(is_current, || {
            content.offsetFromLocation_toLocation(&document_start, &element_end_location)
        })?)
        .map_err(|_| Failure::InvalidSourceAssociation)?;
        if element_start != native_source.start || element_end != native_source.end {
            // 一行placeholder必须对应完整逻辑paragraph，不能拿另一段或局部旧fragment。
            return Err(Failure::InvalidSourceAssociation);
        }
        {
            let document = storage.document().ok_or(Failure::MissingGeometry)?;
            let document = document.borrow();
            if !native_markdown_table_paragraph_tail_is_valid(source, &native_source, |index| {
                document.utf16_code_unit_at(index).ok()
            }) {
                // LF/CRLF可属于完整Native paragraph，额外正文和跨段范围不能被容差隐藏。
                return Err(Failure::InvalidSourceAssociation);
            }
        }
        let frame =
            native_markdown_table_intent_step(is_current, || fragment.layoutFragmentFrame())?;
        let origin = native_markdown_table_intent_step(is_current, || self.textContainerOrigin())?;
        if !frame.origin.y.is_finite()
            || !frame.size.height.is_finite()
            || frame.size.height + 0.01 < minimum_height
        {
            return Err(Failure::MissingGeometry);
        }
        let weak =
            native_markdown_table_intent_step(is_current, || Weak::from_retained(&fragment))?;
        Ok(NativeMarkdownResolvedTableRow {
            source: source.clone(),
            fragment_source: native_source,
            fragment: Some(weak),
            frame: NSRect::new(
                NSPoint::new(frame.origin.x + origin.x, frame.origin.y + origin.y),
                frame.size,
            ),
        })
    }
}

impl PieceBackedMarkdownTextView {
    /// 只绘已发布的有限表格；true要求实际可见cell绘制返回且仍是原cache，不表示模式定位完成。
    pub(super) fn draw_visible_markdown_tables(&self, dirty_rect: NSRect) -> bool {
        native_markdown_trace! {
            eprintln!(
                "[probe] draw tables begin manager={}",
                self.textLayoutManager().is_some()
            );
        }
        // Drawing reads prepared caches only. Parsing, layout and cell activation run in a
        // separate main-queue transaction to avoid reentering TextKit's own drawing stack.
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // A late draw after teardown owns no source or palette and paints no overlay.
            return false;
        };
        let Some(render) = text_storage.render_payload() else {
            // accepted release 已取走全部渲染资源，延迟绘制不得重建它们。
            return false;
        };
        if text_storage.presentation_mode() != MarkdownPresentationMode::InlinePreview {
            // Source presentation uses only native text drawing; no table overlay may conceal raw Markdown.
            return false;
        }
        let Some(request_key) = self.current_table_layout_request_key() else {
            // A detached document has no stable generation/viewport identity for this paint transaction.
            return false;
        };
        // 外层 draw_rect 的 PayloadUseGuard 覆盖此快照至 drop；只增加主线程 Rc 计数，
        // 不复制行/单元格或逐格 retain 属性，且在原生绘制前结束 cache slot 借用。
        let cache = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .map(Rc::clone);
        let Some(cache) = cache.as_ref().filter(|cache| {
            // 同代次数字不证明同一笔记；原生几何读取返回后仍须核对原 owner。
            cache.request_key == request_key
                && self.table_layout_document_is_current(
                    &text_storage,
                    &cache.document_identity,
                    request_key,
                )
        }) else {
            // An obsolete cache stays invisible until the coalesced native layout transaction prepares a replacement.
            return false;
        };
        // 收据只证明本次原cache真实绘制；这些纯代次在Native前固定，不重领几何或source权限。
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let semantic_epoch = text_storage.ivars().font_run_scratch.epoch();
        let mut painted_cells = false;
        let Some(graphics) = NSGraphicsContext::currentContext() else {
            return false; // 没有实际绘图上下文就不能把可见缓存当作已绘制。
        };
        let context = graphics.CGContext();
        // Paint needs only Copy coordinate metadata, never a borrow of the editable field's draft state.
        let active_cell = self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .map(|active| active.coordinate);
        NSGraphicsContext::saveGraphicsState_class();
        // Paint only cached tables intersecting this native dirty rectangle.
        for table in &cache.tables {
            if !NSIntersectsRect(table.frame, dirty_rect) {
                // Distant cached geometry requires no drawing resources for this invalidation.
                continue;
            }
            // Row backgrounds remain separate from text so cell focus can paint above them.
            for row in &table.rows {
                if !NSIntersectsRect(row.frame, dirty_rect) {
                    // Native clipping excludes this row from the current paint pass.
                    continue;
                }
                let background = if row.is_header {
                    // Header tone distinguishes column labels without changing canonical styles.
                    &render.palette.table_header_background
                } else {
                    // Body rows use the regular table background under their immutable display text.
                    &render.palette.table_row_background
                };
                background.setFill();
                NSBezierPath::fillRect(row.frame);
            }
            // Draw only the already prepared visible-cell text; no source parse or native edit occurs.
            for cell in &table.cells {
                if !NSIntersectsRect(cell.frame, dirty_rect) {
                    // A cell outside the dirty clip cannot contribute visible pixels.
                    continue;
                }
                let is_active = active_cell == Some(cell.coordinate);
                if is_active {
                    // Native field editing gets a focused background while its committed source stays unchanged.
                    render.palette.table_active_background.setFill();
                    NSBezierPath::fillRect(cell.frame);
                    painted_cells = true; // 活动字段的原背景实际已画；文字仍由原独立字段负责。
                    continue;
                }
                // 测量、字形命中和绘制共用原可见 CTFrame；绘制期间不再让 NSString 建第二份布局。
                cell.text_layout.draw(cell.text_frame, &context);
                painted_cells = true; // 原格 CTFrame 的同步绘制已经返回。
            }

            render.palette.structure_border.setStroke();
            let grid = NSBezierPath::new();
            grid.setLineWidth(1.0);
            let minimum_y = table.frame.origin.y;
            let maximum_y = table.frame.origin.y + table.frame.size.height;
            // Shared prefix edges ensure adjacent cell borders use identical pixel coordinates.
            for offset in &table.column_offsets {
                let x = table.frame.origin.x + offset;
                grid.moveToPoint(NSPoint::new(x, minimum_y));
                grid.lineToPoint(NSPoint::new(x, maximum_y));
            }
            // Horizontal separators are emitted once per visible row rather than once per cell.
            for row in &table.rows {
                let y = row.frame.origin.y;
                grid.moveToPoint(NSPoint::new(table.frame.origin.x, y));
                grid.lineToPoint(NSPoint::new(
                    table.frame.origin.x + table.frame.size.width,
                    y,
                ));
            }
            if let Some(last_row) = table.rows.last() {
                // The final lower edge closes the visible grid after its last row.
                let y = last_row.frame.origin.y + last_row.frame.size.height;
                grid.moveToPoint(NSPoint::new(table.frame.origin.x, y));
                grid.lineToPoint(NSPoint::new(
                    table.frame.origin.x + table.frame.size.width,
                    y,
                ));
            }
            grid.stroke();
        }
        NSGraphicsContext::restoreGraphicsState_class();
        native_markdown_trace! {
            eprintln!(
                "[probe] draw tables end manager={}",
                self.textLayoutManager().is_some()
            );
        }
        // 只复核原对象的纯状态和同Rc发布槽；不为收据增加Native几何、字符或布局查询。
        // 图层发布方仍须在本函数返回后复核其原Authority，取消只返回false，不能签后继cache。
        painted_cells
            && !self.ivars().textkit_graph_teardown.get()
            && text_storage.ivars().accepts_edits.get()
            && self.ivars().table_input_intent_epoch.get() == input_epoch
            && text_storage.ivars().font_run_scratch.epoch() == semantic_epoch
            && text_storage.presentation_mode() == request_key.presentation_mode
            && text_storage.ivars().table_geometry_epoch.get() == request_key.table_geometry_epoch
            && text_storage.ivars().table_projection_width_key.get() == request_key.text_view_width
            && std::rc::Weak::ptr_eq(
                &text_storage.ivars().document.borrow(),
                &cache.document_identity,
            )
            && self
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, cache))
            && cache.document_identity.upgrade().is_some_and(|document| {
                document.try_borrow().is_ok_and(|document| {
                    document.generation() == request_key.document_generation
                        && document.utf16_len() == request_key.document_length
                })
            })
    }

    /// 同一原权限内取得有限视口key；真实行读取失败不能把0..0签成已知可见范围。
    pub(super) fn current_table_layout_request_key(
        &self,
    ) -> Option<NativeMarkdownTableLayoutRequestKey> {
        #[cfg(feature = "native-markdown-runtime-probe")]
        let _reentry_key_scope = super::viewport_reentry_diagnostic::enter(
            super::viewport_reentry_diagnostic::Phase::RequestKey,
        );
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let capture = self.ivars().folds.capture_selection().ok()?;
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let initial = || {
            capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
        };
        let text_storage = native_markdown_table_intent_step(&initial, || {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::KeyStorage,
            );
            self.ivars().text_storage.load()
        })
        .ok()??;
        let identity = text_storage.ivars().document.borrow().clone();
        let (document_generation, document_length) = {
            let document = identity.upgrade()?;
            if !self.ivars().folds.matches_document(&document) {
                return None; // 同长度的新storage文档不能继承首个Native调用前的原fold票。
            }
            let document = document.borrow();
            (document.generation(), document.utf16_len())
        };
        let presentation_mode = text_storage.presentation_mode();
        let text_view_width = text_storage.ivars().table_projection_width_key.get();
        let table_geometry_epoch = text_storage.ivars().table_geometry_epoch.get();
        let current = || {
            initial()
                && text_storage.ivars().accepts_edits.get()
                && text_storage.presentation_mode() == presentation_mode
                && text_storage.ivars().table_projection_width_key.get() == text_view_width
                && text_storage.ivars().table_geometry_epoch.get() == table_geometry_epoch
                && WeakMarkdownDocument::ptr_eq(&identity, &text_storage.ivars().document.borrow())
        };
        // bounds getter可能让系统兑现估计高度；先结束它，再捕获将交给真实行读取的clip。
        let bounds = native_markdown_table_intent_step(&current, || {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::KeyBounds,
            );
            self.bounds()
        })
        .ok()?;
        if native_markdown_layout_metric(bounds.size.width) != text_view_width {
            return None; // 首次resize尚未兑现原宽度时不拼接两种列宽，也不补签较新的width。
        }
        let visible_rect = native_markdown_table_intent_step(&current, || {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::KeyVisible,
            );
            self.visibleRect()
        })
        .ok()?;
        let source_viewport = native_markdown_table_intent_step(&current, || {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::KeyRead,
            );
            self.read_visible_utf16_window(visible_rect, document_length)
        })
        .ok()?;
        if document_length != 0 && source_viewport.is_empty() {
            return None; // 非空正文缺少真实可见行就是Pending；不能用空key命中或发布缓存。
        }
        if !current() {
            return None; // 局部Native对象析构也可能撤销原票，完成后不领取新代次。
        }
        Some(NativeMarkdownTableLayoutRequestKey {
            document_generation,
            document_length,
            presentation_mode,
            visible_origin_x: native_markdown_layout_metric(visible_rect.origin.x),
            visible_origin_y: native_markdown_layout_metric(visible_rect.origin.y),
            visible_width: native_markdown_layout_metric(visible_rect.size.width),
            visible_height: native_markdown_layout_metric(visible_rect.size.height),
            text_view_width,
            table_geometry_epoch,
            native_viewport_start: source_viewport.start,
            native_viewport_end: source_viewport.end,
        })
    }

    /// 在原生读取返回后核对 storage/文档身份与当前纯 Rust 状态，不把数值代次当成 owner。
    fn table_layout_document_is_current(
        &self,
        text_storage: &PieceBackedMarkdownTextStorage,
        document_identity: &WeakMarkdownDocument,
        request_key: NativeMarkdownTableLayoutRequestKey,
    ) -> bool {
        let Some(current_storage) = self.ivars().text_storage.load() else {
            // view 已失去原 storage，旧快照不可作为迟到绘制或发布权限。
            return false;
        };
        if !std::ptr::eq(&*current_storage, text_storage) {
            // 原生回调换到另一 storage 时，即使其 generation 相同也不能复用。
            return false;
        }
        let current_identity = text_storage.ivars().document.borrow().clone();
        if !std::rc::Weak::ptr_eq(&current_identity, document_identity) {
            // 原 Weak 仍占有旧分配身份，因此地址复用不能把新笔记认作旧笔记。
            return false;
        }
        let Some(document) = current_identity.upgrade() else {
            // Weak 身份相同但正文已释放，仍没有可绘制或发布的业务 owner。
            return false;
        };
        // 所有可能调用原生的读取已经结束；最终检查只读 Rust 状态，不跨 native 持借用。
        let document = document.borrow();
        !self.ivars().textkit_graph_teardown.get()
            && text_storage.presentation_mode() == request_key.presentation_mode
            && document.generation() == request_key.document_generation
            && document.utf16_len() == request_key.document_length
    }

    /// Queues at most one weakly owned foreground table-layout transaction for a changed viewport request.
    pub(super) fn schedule_visible_table_layout_update(&self) {
        // Native hierarchy removal can reenter resize/window callbacks while the old document
        // is still alive. Closing views must never acquire another layout transaction or cache.
        if self.ivars().textkit_graph_teardown.get() {
            // Teardown invalidates every future layout request; queued work must never reconnect the graph.
            return;
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] schedule input_depth={} marked={:?} scheduled={} cache={}",
                self.ivars().textkit_input_depth.get(),
                self.ivars().marked_text_range.get(),
                self.ivars().table_layout_update_scheduled.get(),
                self.ivars().visible_table_layout_cache.borrow().is_some(),
            );
        }
        if self.ivars().textkit_input_depth.get() != 0
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            // Composition/native insertion will schedule again when its geometry becomes stable.
            return;
        }
        if self.ivars().table_layout_update_scheduled.get() {
            // 已排队的弱回调执行时才捕获最新可见范围；此处无需生成随后必被丢弃的key。
            // 尺寸变化可在Native懒布局中同步重入，先合并能避免几何getter反过来再触发布局。
            return;
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            return; // 原owner已结束不能排队；几何尚缺与owner撤销保持区别。
        };
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let current = || {
            capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
        };
        // 真实输入已经使装饰失效时，本次必需进入原 executor；旧 table key 不可能消除此请求。
        // 此分支不读取 viewport/key，避免在字段输入返回栈中为了排队又兑现 Native 几何。
        if !self.ivars().source_decorations_dirty.get() {
            // 首次尚无真实行时没有可签的key；这只取消cache命中资格，不能阻止唯一executor首次布局。
            let request_key = self.current_table_layout_request_key();
            let Some(document_identity) = native_markdown_table_intent_step(&current, || {
                self.ivars().text_storage.load().map(|storage| {
                    // 原生读取之后只保存 Weak 身份；不跨 cache slot 借用调用原生。
                    storage.ivars().document.borrow().clone()
                })
            })
            .ok()
            .flatten() else {
                // 已失去 storage 的 view 不能因旧 key 相同而把过期 cache 当作当前结果。
                return;
            };
            let pending_cell = self.ivars().pending_table_cell_activation.get();
            // 在table cache借用之前读取背景准备权限；MissingLayout只由真实super-draw唤醒一次。
            let blocks_pending = self.preview_blocks_need_preparation();
            if !current() {
                return; // 几何或背景getter内的新输入不能让原栈继续排队或借后继cache。
            }
            if self.ivars().table_layout_update_scheduled.get()
                || self
                    .ivars()
                    .visible_table_layout_cache
                    .borrow()
                    .as_ref()
                    .is_some_and(|cache| {
                        // 精确 owner 与 key 都相同才消除重复调度；新笔记可恰好具有相同代次。
                        Some(cache.request_key) == request_key
                            && std::rc::Weak::ptr_eq(&cache.document_identity, &document_identity)
                            && pending_cell.is_none_or(|coordinate| {
                                cache
                                    .tables
                                    .iter()
                                    .flat_map(|table| &table.cells)
                                    .any(|cell| cell.coordinate == coordinate)
                            })
                            && !self.ivars().source_decorations_dirty.get()
                            && !blocks_pending
                            && self
                                .ivars()
                                .pending_table_cell_activation
                                .request()
                                .is_none()
                            && self.ivars().find_reveal.borrow().is_none()
                            && self.ivars().mode_viewport.borrow().is_none()
                            && {
                                #[cfg(feature = "native-table-row-append")]
                                {
                                    !self.table_append_pending()
                                }
                                #[cfg(not(feature = "native-table-row-append"))]
                                {
                                    true
                                }
                            }
                    })
            {
                // An in-flight or already-current request requires no second dispatch block.
                return;
            }
        }
        if !current() {
            return; // 仅凭 dirty 不能越过原 Doc/输入票；后继编辑与关闭仍撤销本次排队。
        }
        // Coalesce requests without laying out on the current stack. MainThreadBound carries
        // a weak view across dispatch safely without extending its native lifetime.
        if self.ivars().table_layout_update_scheduled.replace(true) {
            // Reentrant request acceptance preserves the first queued transaction.
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            // Thread confinement failure must not leave a permanently scheduled flag or queue a native callback.
            self.ivars().table_layout_update_scheduled.set(false);
            return;
        };
        let weak_view = MainThreadBound::new(Weak::from(self), mtm);
        enqueue_native_markdown_main_task(move || {
            // SAFETY: this closure executes only on the dispatch main queue, establishing main-thread access.
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let Some(text_view) = weak_view.get(mtm).load() else {
                // The originating view is gone; the queued task has no owner or successor authority.
                return;
            };
            text_view.run_scheduled_table_layout_update();
        });
    }

    /// Consumes one scheduled derived-layout request after native editing permits element enumeration.
    pub(super) fn run_scheduled_table_layout_update(&self) {
        // 整个 coalesced 事务都会持有派生 geometry/canonical 副本，不仅 table cache 重建区间。
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let cancelled = self
            .ivars()
            .mode_viewport
            .borrow()
            .as_ref()
            .filter(|request| request.cancelled.get())
            .map(Rc::clone);
        if let Some(cancelled) = cancelled {
            self.remove_mode_viewport_if_same(&cancelled);
        }
        if self
            .ivars()
            .mode_viewport
            .borrow()
            .as_ref()
            .is_some_and(|request| !request.phase.get().is_ready())
        {
            // 同步转换的Native回调不能提前消费半成品；本动作结束时会正常调度唯一任务。
            self.ivars().table_layout_update_scheduled.set(false);
            return;
        }
        // A queued weak callback may still find a native shell retained by AppKit during close.
        // It cannot rebuild presentation or touch its disconnected canonical/storage graph.
        if self.ivars().textkit_graph_teardown.get() {
            // A previously queued executor clears its request after input/graph ownership has ended.
            self.ivars().table_layout_update_scheduled.set(false);
            self.fail_pending_find_reveal();
            self.finish_pending_mode_viewport(false);
            return;
        }
        // TextKit can hold an outer content transaction after our own input-depth guard returns
        // to zero. Its public contract forbids layout or secondary enumeration during that edit.
        if self
            .textContentStorage()
            .is_some_and(|storage| storage.hasEditingTransaction())
        {
            // A later draw/input-completion request may schedule fresh work; never spin or recurse
            // inside an active TextKit editing transaction merely to populate derived overlays.
            self.ivars().table_layout_update_scheduled.set(false);
            self.fail_pending_find_reveal();
            self.finish_pending_mode_viewport(false);
            return;
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] run scheduled={} input_depth={} marked={:?}",
                self.ivars().table_layout_update_scheduled.get(),
                self.ivars().textkit_input_depth.get(),
                self.ivars().marked_text_range.get(),
            );
        }
        if !self.ivars().table_layout_update_scheduled.get() {
            // An explicit input/layout transaction already consumed or cancelled this queued request.
            return;
        }
        if self.ivars().textkit_input_depth.get() != 0
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            // Input completion or unmarkText will request layout again; this callback only
            // clears the coalescing flag while composition is active.
            self.ivars().table_layout_update_scheduled.set(false);
            self.fail_pending_find_reveal();
            self.finish_pending_mode_viewport(false);
            return;
        }
        #[cfg(feature = "native-table-row-append")]
        match self.advance_pending_table_append() {
            NativeMarkdownTableAppendPoll::Progress => {
                // 一个事件仅推进一次 Core；后继只排原 coalesced executor，不重绘或推进输入代次。
                self.ivars().table_layout_update_scheduled.set(false);
                self.schedule_visible_table_layout_update();
                return;
            }
            NativeMarkdownTableAppendPoll::Blocked => return, // 原提交栈将完成当前事务；嵌套栈不追加。
            NativeMarkdownTableAppendPoll::None | NativeMarkdownTableAppendPoll::Reopen => {}
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        if self
            .ivars()
            .pending_table_cell_activation
            .request()
            .is_some()
        {
            // 只记录原异步行请求的首个执行事件，不提前推进请求或读取正文。
            super::probe_call_diagnostic::allocation_audit_once("table_executor_pending_begin");
            #[cfg(feature = "native-raster-surface-probe")]
            // 原异步请求首轮的诊断自行去重；此时没有文档/缓存借用，不创建图层或请求新布局。
            super::probe_call_diagnostic::layer_audit_point("table_executor_pending_begin", self);
        }
        match self.advance_pending_table_cell_request() {
            NativeMarkdownTableCellPoll::Progress => {
                // 一个回调只推进一次真实Core或控制转换；不发重绘/epoch，也不结束原Find/Mode票。
                self.ivars().table_layout_update_scheduled.set(false);
                self.schedule_visible_table_layout_update();
                return;
            }
            NativeMarkdownTableCellPoll::Blocked => return,
            NativeMarkdownTableCellPoll::None => {}
            NativeMarkdownTableCellPoll::Ready => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                super::probe_call_diagnostic::allocation_audit_once(
                    "table_executor_required_ready",
                );
            }
        }
        if let Some(storage) = self.ivars().text_storage.load() {
            // 上方真实TextKit事务/输入门禁与原append/required-cell推进已经结束，才可提交候选来源。
            // 提交会同时推进font/table键；必须先于本批cache构造，完成收据不能见新key配旧geometry。
            storage.finish_source_publication(self);
        }
        let cache_changed = self.refresh_visible_table_layout_cache();
        #[cfg(feature = "native-markdown-runtime-probe")]
        if self
            .ivars()
            .pending_table_cell_activation
            .request()
            .is_some()
        {
            super::probe_call_diagnostic::allocation_audit_once("table_executor_cache_return");
        }
        // Reuse the settled viewport transaction for bounded ruler geometry, never draw-time layout.
        self.refresh_source_gutter();
        #[cfg(feature = "native-markdown-runtime-probe")]
        if self
            .ivars()
            .pending_table_cell_activation
            .request()
            .is_some()
        {
            super::probe_call_diagnostic::allocation_audit_once("table_executor_gutter_return");
        }
        let decorations_changed = self.refresh_source_decorations();
        #[cfg(feature = "native-markdown-runtime-probe")]
        if self
            .ivars()
            .pending_table_cell_activation
            .request()
            .is_some()
        {
            super::probe_call_diagnostic::allocation_audit_once(
                "table_executor_decorations_return",
            );
        }
        // table布局与source cue完成后，复用同一settled事务准备有限系统fragment背景。
        let blocks_changed = self.refresh_preview_blocks();
        #[cfg(feature = "native-markdown-runtime-probe")]
        if self
            .ivars()
            .pending_table_cell_activation
            .request()
            .is_some()
        {
            super::probe_call_diagnostic::allocation_audit_once("table_executor_blocks_return");
        }
        #[cfg(feature = "native-block-fragment-diagnostic")]
        if let Some(storage) = self.ivars().text_storage.load() {
            // 只输出已累计标量，不为诊断触发属性或layout。
            storage.ivars().mode_attribute_diagnostic.report(1);
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] run after refresh manager={} content={} changed={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some(),
                cache_changed
            );
        }
        self.reposition_active_table_cell_editor();
        native_markdown_trace! {
            eprintln!(
                "[probe] run after reposition manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
        let _ = self.activate_pending_table_cell();
        self.finish_pending_table_cell_request();
        #[cfg(feature = "native-table-row-append")]
        self.finish_pending_table_append();
        // 所有同批派生准备结束后只读重核目标；未通过不发布“找到匹配”。
        self.finish_pending_find_reveal();
        self.finish_pending_mode_viewport(true);
        native_markdown_trace! {
            eprintln!(
                "[probe] run after pending manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
        self.ivars().table_layout_update_scheduled.set(false);
        if cache_changed || decorations_changed || blocks_changed {
            // 已发布的有限派生背景只请求当前真实视口重绘，缓存键仍阻止空闲时重复排队。
            self.invalidate_visible_body();
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] run end manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
    }

    /// 取走会话缓存并在 slot 借用结束后释放；局部 owner 析构仍受同步载荷租约保护。
    pub(super) fn invalidate_visible_table_layout_cache(&self) -> bool {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let previous = { self.ivars().visible_table_layout_cache.borrow_mut().take() };
        // 最后一个 Rc 可能销毁全部原生属性；记录是否失效后才在无 RefMut 的栈上 drop。
        let changed = previous.is_some();
        drop(previous);
        changed
    }

    /// Replaces the exact generation/viewport cache once, including stable empty or bounded-failure results.
    pub(super) fn refresh_visible_table_layout_cache(&self) -> bool {
        if self.ivars().table_layout_rebuilding.get() {
            // Reentrant native drawing may inspect the old cache but cannot recursively rebuild it.
            return false;
        }
        self.ivars().table_layout_rebuilding.set(true);
        // The guard remains set across native geometry calls; all finite exits reset it below.
        let changed = (|| {
            if self.ivars().textkit_input_depth.get() != 0
                || self
                    .ivars()
                    .marked_text_range
                    .get()
                    .is_some_and(|range| range.length != 0)
            {
                // Provisional native input keeps source readable and defers table geometry until stable.
                return false;
            }
            let Ok(capture) = self.ivars().folds.capture_selection() else {
                return false; // 首个Native步骤前固定原owner/正文/模式，不能布局后再捕获后继权限。
            };
            let input_epoch = self.ivars().table_input_intent_epoch.get();
            let current = || {
                capture.current()
                    && !self.ivars().textkit_graph_teardown.get()
                    && self.ivars().table_input_intent_epoch.get() == input_epoch
            };
            let Some(text_storage) =
                native_markdown_table_intent_step(&current, || self.ivars().text_storage.load())
                    .ok()
                    .flatten()
            else {
                return false; // 原Native图已结束时不布局、不清理后继图的缓存。
            };
            // 只保留分配身份，不把 canonical owner 加入布局缓存或异步队列。
            let document_identity = text_storage.ivars().document.borrow().clone();
            let Some(document) = document_identity.upgrade() else {
                return false; // 同一弱身份不保活正文，也不能对已销毁Doc签几何。
            };
            if !self.ivars().folds.matches_document(&document) {
                return false;
            }
            let document_length = document.borrow().utf16_len();
            drop(document); // Native布局前结束Doc强边及全部借用。
            let Some((visible_rect, viewport_utf16_window)) =
                native_markdown_table_intent_step(&current, || {
                    self.visible_utf16_window(document_length)
                })
                .ok()
                .flatten()
            else {
                return false; // 唯一布局后仍无真实行，不签空key，也不排第二个布局任务自旋。
            };
            let Some(request_key) = native_markdown_table_intent_step(&current, || {
                self.current_table_layout_request_key()
            })
            .ok()
            .flatten() else {
                return false; // 原有限读取失败与owner撤销都不能成为缓存发布凭据。
            };
            if request_key.native_viewport_start != viewport_utf16_window.start
                || request_key.native_viewport_end != viewport_utf16_window.end
                || request_key.visible_origin_x
                    != native_markdown_layout_metric(visible_rect.origin.x)
                || request_key.visible_origin_y
                    != native_markdown_layout_metric(visible_rect.origin.y)
                || request_key.visible_width
                    != native_markdown_layout_metric(visible_rect.size.width)
                || request_key.visible_height
                    != native_markdown_layout_metric(visible_rect.size.height)
            {
                return false; // key读取期间clip又变动时丢弃整对几何，不能拼接两个时刻的窗口。
            }
            let mut request_key = request_key;
            native_markdown_trace! {
                eprintln!(
                    "[probe] refresh key={:?} viewport={:?} mode={:?}",
                    request_key, viewport_utf16_window, request_key.presentation_mode
                );
            }
            let pending_cell = self.ivars().pending_table_cell_activation.get();
            let mutation_pending = self.ivars().table_mutation_anchor.borrow().is_some();
            let reveal = self.pending_find_reveal_context();
            let mode = self.pending_mode_viewport_context();
            let cache_matches = reveal.is_none()
                && mode.is_none()
                && !mutation_pending
                && self
                    .ivars()
                    .visible_table_layout_cache
                    .borrow()
                    .as_ref()
                    .is_some_and(|cache| {
                        cache.request_key == request_key
                            && std::rc::Weak::ptr_eq(&cache.document_identity, &document_identity)
                            && pending_cell.is_none_or(|coordinate| {
                                cache
                                    .tables
                                    .iter()
                                    .flat_map(|table| &table.cells)
                                    .any(|cell| cell.coordinate == coordinate)
                            })
                    });
            if cache_matches {
                return false;
            }
            let previous = self
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .filter(|cache| std::rc::Weak::ptr_eq(&cache.document_identity, &document_identity))
                .map(Rc::clone);
            let mutation = self.ivars().table_mutation_anchor.borrow_mut().take();
            let mutation = mutation.and_then(|anchor| {
                if anchor.matches(
                    &document_identity,
                    request_key,
                    self.ivars().table_input_intent_epoch.get(),
                ) {
                    Some(anchor)
                } else {
                    // 宽度/模式/owner/代次变化撤销旧动作；不能拿新viewport冒充连续性成功。
                    self.table_edit_diagnostic("mutation_identity_expired_cancelled");
                    None
                }
            });
            // 此局部epoch覆盖prepare返回至cache发布，不能只在slot.take时认证一次。
            let preparation_input_epoch = self.ivars().table_input_intent_epoch.get();
            self.table_edit_diagnostic("executor_consume");
            let mut prepare_window = viewport_utf16_window.clone();
            if pending_cell.is_some()
                && let Some(row) = self.pending_table_cell_source_row()
                && let Some(window) =
                    native_markdown_bounded_table_layout_range(&prepare_window, &row)
            {
                // 这里只消费同一RequiredRow Ready的真实源行；不重新materialize/复制整表来找ordinal。
                prepare_window = window;
            }
            if !self.table_input_intent_is_current(
                &text_storage,
                &document_identity,
                request_key,
                preparation_input_epoch,
            ) {
                // scope helper的Native弱加载可能重入新输入；进入batch前守原epoch，不能由Viewport分支重采权限。
                return false;
            }
            let prepared = self.prepare_stable_table_batch(
                &text_storage,
                &document_identity,
                request_key,
                prepare_window,
                previous.as_deref(),
                if let Some(reveal) = reveal.as_ref() {
                    NativeMarkdownTablePreparationTarget::Find(reveal)
                } else if let Some(mode) = mode.as_ref() {
                    NativeMarkdownTablePreparationTarget::Mode(mode)
                } else if let Some(mutation) = mutation {
                    NativeMarkdownTablePreparationTarget::Mutation(mutation)
                } else {
                    NativeMarkdownTablePreparationTarget::Viewport
                },
            );
            let (utf16_window, tables, final_key, viewport_utf16_window) = match prepared {
                Ok(value) => value,
                Err(NativeMarkdownStableTableFailure::InputIntentExpired) => {
                    // 新输入已接管viewport；不发布literal清高度，也不重试/回滚新clip。
                    self.table_edit_diagnostic("preparation_input_expired_cancelled");
                    return false;
                }
                Err(reason) => {
                    #[cfg(not(feature = "native-table-edit-diagnostic"))]
                    let _ = reason;
                    // 失败只保原文安全，不标记preview完成；不会在本栈循环/等待布局。
                    #[cfg(feature = "native-table-edit-diagnostic")]
                    eprintln!("table_candidate_failure={reason:?} preview_complete=false");
                    let Some(current) = self.current_table_layout_request_key() else {
                        return false;
                    };
                    if !self.table_layout_document_is_current(
                        &text_storage,
                        &document_identity,
                        request_key,
                    ) || current.text_view_width != request_key.text_view_width
                    {
                        return false;
                    }
                    if reveal
                        .as_ref()
                        .is_some_and(|target| !target.is_current(self))
                        || mode.as_ref().is_some_and(|target| !target.is_current(self))
                    {
                        return false; // 原生失败检查重入新查询后，旧目标不能清后继高度。
                    }
                    return self.publish_literal_table_fallback(
                        &text_storage,
                        &document_identity,
                        current,
                    );
                }
            };
            request_key = final_key;
            if reveal
                .as_ref()
                .is_some_and(|target| !target.is_current(self))
                || mode.as_ref().is_some_and(|target| !target.is_current(self))
            {
                // 目标权限在Native准备中失效，旧批次不发布或回滚后继几何。
                return false;
            }
            let text_view_width = self.bounds().size.width;
            if self.ivars().table_input_intent_epoch.get() != preparation_input_epoch
                || reveal
                    .as_ref()
                    .is_some_and(|target| !target.is_current(self))
                || mode.as_ref().is_some_and(|target| !target.is_current(self))
            {
                // bounds getter重入的新事件只撤销本批局部cache，不碰其新几何。
                return false;
            }
            #[cfg(not(feature = "native-markdown-runtime-probe"))]
            let _ = (&utf16_window, &viewport_utf16_window);
            // 原生测量可能重入编辑、滚动或关闭；只有原始代次/视口仍有效才发布。
            // 失效时局部 tables 连同容量与原生属性立即 drop，不恢复旧会话权限。
            if self.ivars().textkit_graph_teardown.get()
                || self.current_table_layout_request_key() != Some(request_key)
                || !self.table_layout_document_is_current(
                    &text_storage,
                    &document_identity,
                    request_key,
                )
                || self.ivars().textkit_input_depth.get() != 0
                || self
                    .ivars()
                    .marked_text_range
                    .get()
                    .is_some_and(|range| range.length != 0)
            {
                // 外层 payload 租约不授权写入过期几何，也不替代资源完成收据。
                if self.ivars().table_input_intent_epoch.get() != preparation_input_epoch
                    || reveal
                        .as_ref()
                        .is_some_and(|target| !target.is_current(self))
                    || mode.as_ref().is_some_and(|target| !target.is_current(self))
                {
                    // 最终native权限getter内重入时同样不得清理后继事件的高度。
                    return false;
                }
                return self.publish_literal_table_fallback(
                    &text_storage,
                    &document_identity,
                    request_key,
                );
            }
            if self.ivars().table_input_intent_epoch.get() != preparation_input_epoch
                || reveal
                    .as_ref()
                    .is_some_and(|target| !target.is_current(self))
                || mode.as_ref().is_some_and(|target| !target.is_current(self))
            {
                // 其他条件均相同的新物理意图也不能被旧局部结果覆盖。
                return false;
            }
            // 一次布局只构造一个不可变 Rc 投影；旧 owner 移出借用后再释放原生字段。
            let replacement = Rc::new(NativeMarkdownVisibleTableLayoutCache {
                document_identity,
                document_generation: request_key.document_generation,
                #[cfg(feature = "native-markdown-runtime-probe")]
                utf16_window,
                #[cfg(feature = "native-markdown-runtime-probe")]
                viewport_utf16_window,
                text_view_width,
                tables,
                request_key,
            });
            let previous = self
                .ivars()
                .visible_table_layout_cache
                .replace(Some(replacement));
            drop(previous);
            if let Some(reveal) = reveal.as_ref() {
                reveal.mark_prepared(self);
            }
            if let Some(mode) = mode.as_ref()
                && mode.is_current(self)
            {
                mode.request.prepared.set(true);
            }
            true
        })();
        self.ivars().table_layout_rebuilding.set(false);
        changed
    }

    /// 不稳定native几何只能退回完整literal；精确负缓存防止每帧重复失败布局。
    fn publish_literal_table_fallback(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        identity: &WeakMarkdownDocument,
        request: NativeMarkdownTableLayoutRequestKey,
    ) -> bool {
        if self.ivars().textkit_graph_teardown.get()
            || storage.ivars().table_geometry_epoch.get() != request.table_geometry_epoch
            || self
                .textContentStorage()
                .is_some_and(|content| content.hasEditingTransaction())
            || !self.table_layout_document_is_current(storage, identity, request)
        {
            // 关闭、重入新投影或编辑中都没有撤销权限，不能清掉另一批有效高度。
            return false;
        }
        if storage
            .install_table_row_heights(
                identity,
                request.document_generation,
                request.text_view_width,
                Vec::new(),
            )
            .is_none()
        {
            // 清除失败不伪报完成；后续正常输入/布局仍可显式恢复。
            return false;
        }
        let Some(current) = self.current_table_layout_request_key() else {
            // 当前文档已结束，旧callback不能再创建cache owner。
            return false;
        };
        if !self.table_layout_document_is_current(storage, identity, current) {
            // 原生通知若切换了owner，负缓存也不得跨笔记发布。
            return false;
        }
        let replacement = Rc::new(NativeMarkdownVisibleTableLayoutCache {
            document_identity: identity.clone(),
            document_generation: current.document_generation,
            #[cfg(feature = "native-markdown-runtime-probe")]
            utf16_window: 0..0,
            #[cfg(feature = "native-markdown-runtime-probe")]
            viewport_utf16_window: 0..0,
            text_view_width: self.bounds().size.width,
            tables: Vec::new(),
            request_key: current,
        });
        // 空table缓存仅表示当前显示literal，不代表preview或最终功能验收通过。
        let previous = self
            .ivars()
            .visible_table_layout_cache
            .replace(Some(replacement));
        drop(previous);
        true
    }

    /// 只测量有界source窗口及固定overscan；最终位置由同批无ensure矩阵统一赋予。
    pub(super) fn build_visible_table_layouts(
        &self,
        text_storage: &PieceBackedMarkdownTextStorage,
        snapshot: &NativeMarkdownTablePresentationSnapshot,
        viewport_utf16_window: &std::ops::Range<usize>,
        _visible_rect: NSRect,
        previous_cache: Option<&NativeMarkdownVisibleTableLayoutCache>,
    ) -> Vec<NativeMarkdownVisibleTableLayout> {
        if native_markdown_table_preparation_budget(snapshot.tables(), viewport_utf16_window)
            .is_none()
        {
            // 必须先验证整批rows/cells/source/列宽容量，再创建任何native cell或with_capacity数组。
            // 超限交给调用方发布空投影并保完整literal；这只是安全中间态，不是大表parity通过。
            return Vec::new();
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] build tables begin manager={} content={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some()
            );
        }
        // Reserve the document inset and one row of vertical overscan before selecting table fragments.
        let text_origin = self.textContainerOrigin();
        let inset = self.textContainerInset();
        let available_width = (self.bounds().size.width - text_origin.x - inset.width).max(0.0);
        let mut layouts = Vec::new();
        // Semantic parsing may include overscan tables; geometry is acquired only for visible intervals.
        for (table_index, table) in snapshot.tables().iter().enumerate() {
            // A parser overscan may contain distant tables. Reject those before asking
            // TextKit for their geometry: ensuring offscreen EOF fragments can move its
            // transient viewport range away from the actual scroll clip.
            if table.utf16_range.start >= viewport_utf16_window.end
                || table.utf16_range.end <= viewport_utf16_window.start
            {
                continue;
            }
            let visible_row_range =
                native_markdown_visible_table_row_range(&table.rows, viewport_utf16_window);
            native_markdown_trace! {
                eprintln!(
                    "[probe] table range={:?} rows={} visible_rows={:?}",
                    table.utf16_range,
                    table.rows.len(),
                    visible_row_range
                );
            }
            if visible_row_range.is_empty() {
                // A table with no current visible/overscan rows contributes no native cell resources.
                continue;
            }
            // 源viewport已选出有限行；测量阶段只保存索引，不逐行ensure或混入旧geometry。
            let source_rows: Vec<usize> = visible_row_range.collect();
            // Establish current semantic column count and retain only one bounded width per column.
            let column_count = native_markdown_table_column_count(table).unwrap_or(0);
            if column_count == 0 {
                // Empty parsed cells cannot produce a meaningful grid or editing coordinate.
                continue;
            }
            let mut natural_widths = vec![NATIVE_MARKDOWN_TABLE_MINIMUM_COLUMN_WIDTH; column_count];
            if let Some(previous_table) = previous_cache.and_then(|cache| {
                // Column widths can be reused only within the same generation, width and table identity.
                (cache.document_generation == snapshot.document_generation
                    && (cache.text_view_width - self.bounds().size.width).abs() < 0.5)
                    .then(|| {
                        cache.tables.iter().find(|candidate| {
                            candidate.table_utf16_start == table.utf16_range.start
                                && candidate.column_offsets.len() == column_count + 1
                        })
                    })?
            }) {
                // Preserve columns already measured in another visible part of this same table.
                for (column_index, offsets) in previous_table.column_offsets.windows(2).enumerate()
                {
                    natural_widths[column_index] = offsets[1] - offsets[0];
                }
            }
            // Capacity depends only on selected visible/overscan rows, never the whole table row count.
            let visible_cell_capacity = source_rows.iter().fold(0_usize, |count, row_index| {
                count.saturating_add(
                    table
                        .rows
                        .get(*row_index)
                        .map_or(0, |row| row.cell_index_range.len()),
                )
            });
            let mut prepared_cells = Vec::with_capacity(visible_cell_capacity);
            // 所有排版只消费原窗口已准入的可见格，不扩大上游源读取预算。
            for row_index in &source_rows {
                let Some(row) = table.rows.get(*row_index) else {
                    // A missing semantic row cannot justify a stale source-range read.
                    continue;
                };
                // Prepare each visible cell once so drawing can use immutable attributed text only.
                for cell_index in row.cell_index_range.clone() {
                    let Some(cell) = table.cells.get(cell_index) else {
                        // Defensive malformed-prototype handling leaves the source cell readable.
                        continue;
                    };
                    let Some((attributed_text, source_spans)) =
                        text_storage.table_cell_attributed_text(snapshot, table_index, cell_index)
                    else {
                        // A bounded source/style failure creates no partial native cell object.
                        continue;
                    };
                    let Some(natural) = super::table_editor::NativeMarkdownTableCellTextLayout::new(
                        &attributed_text,
                        NATIVE_MARKDOWN_TABLE_MAXIMUM_COLUMN_WIDTH,
                    ) else {
                        continue; // 原生字体排版未完整覆盖本格，整表完整性检查会拒绝发布。
                    };
                    natural_widths[cell.column_index] = natural_widths[cell.column_index].max(
                        natural.size.width + NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING * 2.0,
                    );
                    // 自然宽度只参与列分配；此临时 frame 当格归还，不与最终布局一起缓存。
                    drop(natural);
                    prepared_cells.push((cell_index, *row_index, attributed_text, source_spans));
                }
            }
            if prepared_cells.len() != visible_cell_capacity {
                // 任一格准备失败就放弃本表，不能以部分overlay隐藏完整source行。
                continue;
            }
            let column_widths =
                native_markdown_table_column_widths(&natural_widths, available_width);
            if column_widths.len() != column_count {
                // A collapsed/invalid horizontal viewport has no consistent cell grid to expose.
                continue;
            }
            let mut column_offsets = Vec::with_capacity(column_count + 1);
            column_offsets.push(0.0);
            // Prefix sums define one shared edge coordinate for neighboring cells and grid lines.
            for width in &column_widths {
                let next = column_offsets.last().copied().unwrap_or(0.0) + width;
                column_offsets.push(next);
            }
            let table_width = column_offsets.last().copied().unwrap_or(0.0);
            // natural width只能决定最终列宽；缩列后必须用同一份可见文字重新测量完整高度。
            let prepared_cells: Option<Vec<_>> = prepared_cells
                .into_iter()
                .map(|(cell_index, row_index, source, source_spans)| {
                    let cell = table.cells.get(cell_index)?;
                    // 极窄视口不能以零宽或负宽的裁剪框覆盖原始内容。
                    let width = native_markdown_table_content_width(
                        *column_widths.get(cell.column_index)?,
                    )?;
                    let attributed =
                        native_markdown_wrapped_table_cell_text(&source, cell.alignment);
                    let layout = super::table_editor::NativeMarkdownTableCellTextLayout::new(
                        &attributed,
                        width,
                    )?;
                    // 原唯一有限属性值移入可见格；frame 与该值共享同一显示来源，不复制 String。
                    Some((
                        cell_index,
                        row_index,
                        Rc::new(layout),
                        Rc::from(source_spans),
                        attributed,
                    ))
                })
                .collect();
            let Some(prepared_cells) = prepared_cells else {
                // 尚无完整height批次时source仍可读，禁止部分overlay遮住没有准备的格。
                continue;
            };
            let rows: Vec<NativeMarkdownVisibleTableRowLayout> = source_rows
                .iter()
                .filter_map(|row_index| {
                    // 同一行由最终列宽下最高的完整单元格撑开，不能复用旧38点fragment的高度。
                    let row = table.rows.get(*row_index)?;
                    let height = native_markdown_measured_table_row_height(
                        prepared_cells
                            .iter()
                            .filter(|(_, index, _, _, _)| index == row_index)
                            .map(|(_, _, layout, _, _)| layout.size.height),
                    )?;
                    Some(NativeMarkdownVisibleTableRowLayout {
                        row_index: *row_index,
                        source_utf16_range: row.utf16_range.clone(),
                        frame: NSRect::new(
                            NSPoint::new(text_origin.x, 0.0),
                            NSSize::new(table_width, height),
                        ),
                        is_header: row.is_header,
                    })
                })
                .collect();
            let minimum_y = rows
                .iter()
                .map(|row| row.frame.origin.y)
                .fold(f64::INFINITY, f64::min);
            let maximum_y = rows
                .iter()
                .map(|row| row.frame.origin.y + row.frame.size.height)
                .fold(f64::NEG_INFINITY, f64::max);
            let cells = prepared_cells
                .into_iter()
                .filter_map(
                    |(cell_index, row_index, text_layout, source_spans, attributed_text)| {
                        // Translate each prepared cell into the current shared row/column coordinate system.
                        let prototype = table.cells.get(cell_index)?;
                        let row = table.rows.get(row_index)?;
                        let row_frame = rows
                            .iter()
                            .find(|candidate| candidate.row_index == row_index)?
                            .frame;
                        let column_start = *column_offsets.get(prototype.column_index)?;
                        let column_end = *column_offsets.get(prototype.column_index + 1)?;
                        let frame = NSRect::new(
                            NSPoint::new(text_origin.x + column_start, row_frame.origin.y),
                            NSSize::new(column_end - column_start, row_frame.size.height),
                        );
                        let available_text_width = (frame.size.width
                            - NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING * 2.0)
                            .max(0.0);
                        let available_text_height = (frame.size.height
                            - NATIVE_MARKDOWN_TABLE_CELL_VERTICAL_PADDING * 2.0)
                            .max(0.0);
                        // 测量和绘制使用完整content rect；对齐由同一个paragraph属性决定。
                        let text_width = available_text_width.max(1.0);
                        let text_height = available_text_height.max(1.0);
                        let text_x = frame.origin.x + NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING;
                        Some(NativeMarkdownVisibleTableCellLayout {
                            coordinate: NativeMarkdownTableCellCoordinate {
                                table_utf16_start: table.utf16_range.start,
                                row_index,
                                column_index: prototype.column_index,
                            },
                            source_utf16_range: prototype.utf16_range.clone(),
                            frame,
                            text_frame: NSRect::new(
                                NSPoint::new(
                                    text_x,
                                    frame.origin.y + (frame.size.height - text_height) / 2.0,
                                ),
                                NSSize::new(text_width, text_height),
                            ),
                            is_header: row.is_header,
                            alignment: prototype.alignment,
                            text_layout,
                            source_spans,
                            attributed_text,
                        })
                    },
                )
                .collect();
            // Retain only the completed visible projection; the original Markdown remains in the piece tree.
            layouts.push(NativeMarkdownVisibleTableLayout {
                table_utf16_start: table.utf16_range.start,
                delimiter_utf16_range: table.delimiter_utf16_range.clone(),
                frame: NSRect::new(
                    NSPoint::new(text_origin.x, minimum_y),
                    NSSize::new(table_width, (maximum_y - minimum_y).max(0.0)),
                ),
                rows,
                column_offsets,
                cells,
            });
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] build tables end manager={} content={} layouts={}",
                self.textLayoutManager().is_some(),
                self.textContentStorage().is_some(),
                layouts.len(),
            );
        }
        layouts
    }

    /// 唯一有限viewport布局结束后配对读取实际clip与源行；调用方不能继续使用布局前的滚动位置。
    pub(super) fn visible_utf16_window(
        &self,
        document_length: usize,
    ) -> Option<(NSRect, std::ops::Range<usize>)> {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        let capture = self.ivars().folds.capture_selection().ok()?;
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let current = || {
            capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().table_input_intent_epoch.get() == input_epoch
        };
        let result = (|| {
            if document_length != 0 {
                let manager =
                    native_markdown_table_intent_step(&current, || self.textLayoutManager())
                        .ok()??;
                let expected = native_markdown_table_intent_step(&current, || {
                    self.ivars().text_layout_manager.get().and_then(Weak::load)
                })
                .ok()??;
                if !std::ptr::eq(&*manager, &*expected) {
                    return None; // 原view的公开manager与构造backlink不同，不替换成后继图布局。
                }
                let viewport = native_markdown_table_intent_step(&current, || {
                    manager.textViewportLayoutController()
                })
                .ok()?;
                let actual =
                    native_markdown_table_intent_step(&current, || viewport.textLayoutManager())
                        .ok()??;
                if !std::ptr::eq(&*actual, &*manager) {
                    return None; // viewport必须属于已取得的同一manager，不能只凭相同Doc长度关联。
                }
                native_markdown_table_intent_step(&current, || viewport.layoutViewport()).ok()?;
            }
            // layoutViewport和bounds都可能兑现估计高度并移动clip；两者结束后才保存实际矩形。
            // 空Doc只走这些有限几何getter，不布局或枚举不存在的正文行。
            native_markdown_table_intent_step(&current, || self.bounds()).ok()?;
            let visible_rect =
                native_markdown_table_intent_step(&current, || self.visibleRect()).ok()?;
            let window = if document_length == 0 {
                0..0
            } else {
                let window = native_markdown_table_intent_step(&current, || {
                    self.read_visible_utf16_window(visible_rect, document_length)
                })
                .ok()?;
                if window.is_empty() {
                    return None; // 未得真实行保留Pending，不返回一个看似已知为空的视口。
                }
                window
            };
            Some((visible_rect, window))
        })(); // manager/viewport等局部Native claims先结束，最后只核原capture，不领取新票。
        current().then_some(result).flatten()
    }

    /// 只读取 clip 两侧真实排版行；完整 paragraph 或旧 viewport envelope 都不是可见源范围。
    pub(super) fn read_visible_utf16_window(
        &self,
        visible_rect: NSRect,
        document_length: usize,
    ) -> std::ops::Range<usize> {
        #[cfg(feature = "native-markdown-runtime-probe")]
        let _reentry_read_scope = super::viewport_reentry_diagnostic::enter(
            super::viewport_reentry_diagnostic::Phase::ReadVisible,
        );
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let diagnostic_stage = Cell::new(VisibleLineReadStage::Admission);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let diagnostic_source = Cell::new(false);
        #[cfg(feature = "native-markdown-runtime-probe")]
        let diagnostic_bottom = Cell::new(false);
        macro_rules! visible_line_stage {
            ($stage:ident) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                diagnostic_stage.set(VisibleLineReadStage::$stage);
            };
        }
        if document_length == 0 || !mode_rect_is_finite(visible_rect) {
            // 空正文没有源行；非法 clip 也不能靠段落或固定字符数猜一个可见窗口。
            return 0..0;
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            // 首个 Native claim 之前固定原 owner、文档、代次和模式；关闭或交接不能借新权限。
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_visible_line_read_failure(VisibleLineReadStage::Owner, false, false);
            return 0..0;
        };
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let result = (|| -> Option<std::ops::Range<usize>> {
            visible_line_stage!(Storage);
            let storage = native_markdown_table_intent_step(&|| capture.current(), || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadStorage,
                );
                self.ivars().text_storage.load()
            })
            .ok()??;
            visible_line_stage!(Document);
            let identity = storage.ivars().document.borrow().clone();
            let (generation, mode, width_key) = {
                let document = identity.upgrade()?;
                let document_ref = document.borrow();
                if document_ref.utf16_len() != document_length
                    || !self.ivars().folds.matches_document(&document)
                {
                    // 调用者旧长度和另一 storage 的同代次数值都不能解释当前 Native 坐标。
                    return None;
                }
                (
                    document_ref.generation(),
                    storage.presentation_mode(),
                    storage.ivars().table_projection_width_key.get(),
                )
            };
            #[cfg(feature = "native-markdown-runtime-probe")]
            diagnostic_source.set(mode == MarkdownPresentationMode::Source);
            let current = || {
                capture.current()
                    && !self.ivars().textkit_graph_teardown.get()
                    && self.ivars().table_input_intent_epoch.get() == input_epoch
                    && storage.ivars().accepts_edits.get()
                    && storage.presentation_mode() == mode
                    && storage.ivars().table_projection_width_key.get() == width_key
                    && WeakMarkdownDocument::ptr_eq(&identity, &storage.ivars().document.borrow())
                    && identity.upgrade().is_some_and(|document| {
                        let document = document.borrow();
                        document.generation() == generation
                            && document.utf16_len() == document_length
                    })
            };
            visible_line_stage!(Manager);
            let manager = native_markdown_table_intent_step(&current, || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadManager,
                );
                self.textLayoutManager()
            })
            .ok()??;
            visible_line_stage!(ExpectedManager);
            let expected = native_markdown_table_intent_step(&current, || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadExpectedManager,
                );
                self.ivars().text_layout_manager.get().and_then(Weak::load)
            })
            .ok()??;
            visible_line_stage!(ManagerAssociation);
            if !std::ptr::eq(&*manager, &*expected)
                || native_markdown_table_intent_step(&current, || {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    let _phase = super::viewport_reentry_diagnostic::enter(
                        super::viewport_reentry_diagnostic::Phase::ReadLayoutQueue,
                    );
                    manager.layoutQueue()
                })
                .ok()?
                .is_some()
            {
                // 只观察本图同步布局；后台布局或其他 manager 的片段没有本次发布权限。
                return None;
            }
            visible_line_stage!(Content);
            let content = native_markdown_table_intent_step(&current, || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadContent,
                );
                manager.textContentManager()
            })
            .ok()??;
            visible_line_stage!(Editing);
            if native_markdown_table_intent_step(&current, || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadEditing,
                );
                content.hasEditingTransaction()
            })
            .ok()?
            {
                // 活事务内的 element/line 可以整体替换；等待既有完成事件，不主动 ensure。
                return None;
            }
            visible_line_stage!(Geometry);
            let origin = native_markdown_table_intent_step(&current, || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadOrigin,
                );
                let value = self.textContainerOrigin();
                #[cfg(feature = "native-markdown-runtime-probe")]
                super::viewport_reentry_diagnostic::returned_point(value);
                value
            })
            .ok()?;
            let bounds = native_markdown_table_intent_step(&current, || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadBounds,
                );
                let value = self.bounds();
                #[cfg(feature = "native-markdown-runtime-probe")]
                super::viewport_reentry_diagnostic::returned_rect(value);
                value
            })
            .ok()?;
            visible_line_stage!(Viewport);
            #[cfg(feature = "native-markdown-runtime-probe")]
            let trace_viewport_failure = |reason: &'static str, observed: Option<NSRect>| {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static COUNT: AtomicUsize = AtomicUsize::new(0);
                if COUNT.fetch_add(1, Ordering::Relaxed) < 8 {
                    // 仅消费原栈已取得的几何；不为日志补getter、Native操作或重排。
                    eprintln!(
                        "native_visible_viewport_rejected reason={reason} bounds={bounds:?} width_key={width_key} measured_width={} supplied={visible_rect:?} observed={observed:?} source={} formal=false",
                        native_markdown_layout_metric(bounds.size.width),
                        mode == MarkdownPresentationMode::Source,
                    );
                }
            };
            if native_markdown_layout_metric(bounds.size.width) != width_key {
                // 原短路顺序保持：宽度已不匹配时，绝不额外调用visibleRect补日志。
                #[cfg(feature = "native-markdown-runtime-probe")]
                trace_viewport_failure("width_key", None);
                return None;
            }
            let observed_visible = match native_markdown_table_intent_step(&current, || {
                #[cfg(feature = "native-markdown-runtime-probe")]
                let _phase = super::viewport_reentry_diagnostic::enter(
                    super::viewport_reentry_diagnostic::Phase::ReadClip,
                );
                let value = self.visibleRect();
                #[cfg(feature = "native-markdown-runtime-probe")]
                super::viewport_reentry_diagnostic::returned_rect(value);
                value
            }) {
                Ok(observed) => observed, // 仍只调用原来的单次getter，不因失败再取一个新矩形。
                Err(_) => {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_viewport_failure("native_step_revoked", None);
                    return None;
                }
            };
            if observed_visible != visible_rect {
                // setter或滚动已改变坐标系，旧clip不能和后继行几何混合使用。
                #[cfg(feature = "native-markdown-runtime-probe")]
                trace_viewport_failure("visible_rect_changed", Some(observed_visible));
                return None;
            }
            visible_line_stage!(Provider);
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let document_range =
                native_markdown_table_intent_step(&current, || provider.documentRange()).ok()?;
            let document_start =
                native_markdown_table_intent_step(&current, || document_range.location()).ok()?;
            let source_range = |range: &NSTextRange| -> Option<std::ops::Range<usize>> {
                let from = native_markdown_table_intent_step(&current, || range.location()).ok()?;
                let to =
                    native_markdown_table_intent_step(&current, || range.endLocation()).ok()?;
                let from = usize::try_from(
                    native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &from)
                    })
                    .ok()?,
                )
                .ok()?;
                let to = usize::try_from(
                    native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &to)
                    })
                    .ok()?,
                )
                .ok()?;
                // 原生范围必须完整属于原 canonical 根，不把越界端点 clamp 成合法关联。
                (from <= to && to <= document_length).then_some(from..to)
            };
            struct VisibleLine {
                fragment: Retained<objc2_app_kit::NSTextLayoutFragment>,
                element: Retained<objc2_app_kit::NSTextElement>,
                line: Retained<objc2_app_kit::NSTextLineFragment>,
                frame: NSRect,
                line_bounds: NSRect,
                local_range: NSRange,
                element_source: std::ops::Range<usize>,
                fragment_source: std::ops::Range<usize>,
                source: std::ops::Range<usize>,
            }
            let reverse_candidate = |seed: &ProtocolObject<dyn objc2_app_kit::NSTextLocation>| {
                const MAX_VISIBLE_LINE_REVERSE_FRAGMENTS: usize = 128;
                let visited = Cell::new(0_usize);
                let selected = RefCell::new(None);
                let block = block2::RcBlock::new(
                    |pointer: NonNull<objc2_app_kit::NSTextLayoutFragment>| {
                        if !current()
                            || visited.get() >= MAX_VISIBLE_LINE_REVERSE_FRAGMENTS
                            || selected.borrow().is_some()
                        {
                            // 撤权、预算已满或已有首个候选都立即停止，不靠下一 callback 重试。
                            return objc2::runtime::Bool::NO;
                        }
                        visited.set(visited.get() + 1);
                        let candidate = (|| -> Option<bool> {
                            // SAFETY: 公开同步枚举只在 callback 内借出活 fragment，不保存裸指针。
                            let fragment = unsafe { pointer.as_ref() };
                            let state =
                                native_markdown_table_intent_step(&current, || fragment.state())
                                    .ok()?;
                            if state != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable {
                                // 不向 estimated/未布局片段索取 frame 或 line，只在固定预算内跳过。
                                return Some(true);
                            }
                            let actual_manager =
                                native_markdown_table_intent_step(&current, || {
                                    fragment.textLayoutManager()
                                })
                                .ok()??;
                            if !std::ptr::eq(&*actual_manager, &*manager) {
                                // 不能以当前 view 的 clip 解释另一原生图中的真实 frame。
                                return None;
                            }
                            let frame = native_markdown_table_intent_step(&current, || {
                                fragment.layoutFragmentFrame()
                            })
                            .ok()?;
                            match visible_line_reverse_frame(frame, visible_rect, origin)? {
                                VisibleLineReverseFrame::Above => {
                                    return Some(false); // 已越过 clip 顶部，不扫描更早的离屏段落。
                                }
                                VisibleLineReverseFrame::Below => {
                                    return Some(true); // clip 下方只略过，不授予任何可见源权限。
                                }
                                VisibleLineReverseFrame::Intersects => {} // 首个真实相交候选进入原单行验证。
                            }
                            let retained = native_markdown_table_intent_step(&current, || {
                                // SAFETY: callback 的活指针尚有效，仅为本次行复核增加局部 +1 claim。
                                unsafe { Retained::retain(pointer.as_ptr()) }
                            })
                            .ok()??;
                            *selected.borrow_mut() = Some((retained, state, frame));
                            Some(false) // 首个真实相交候选确定后停止，不收集其他 fragment 或正文。
                        })();
                        if !current() || visited.get() == MAX_VISIBLE_LINE_REVERSE_FRAGMENTS {
                            return objc2::runtime::Bool::NO;
                        }
                        candidate.unwrap_or(false).into()
                    },
                );
                let enumerated = native_markdown_table_intent_step(&current, || {
                    // Reverse 从 seed 所在片段之前开始；不请求布局、估计或合成额外行。
                    manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                        Some(seed),
                        objc2_app_kit::NSTextLayoutFragmentEnumerationOptions::Reverse,
                        &block,
                    )
                })
                .ok()?;
                drop(enumerated);
                drop(block); // 原生 callback/返回位置先释放，slot 借用从未跨过 Native 调用。
                let selected = selected.into_inner();
                #[cfg(feature = "native-markdown-runtime-probe")]
                trace_visible_line_reverse(
                    visited.get(),
                    selected.is_some(),
                    mode == MarkdownPresentationMode::Source,
                );
                current().then_some(selected).flatten()
            };
            let viewport_edge_candidate = |bottom: bool| {
                const MAX_VISIBLE_LINE_VIEWPORT_FRAGMENTS: usize = 256;
                let selected = (|| {
                    // 点查询可以返回未排版 placeholder；只有原 controller 声明的已有 viewport 可作另一个有限入口。
                    let viewport = native_markdown_table_intent_step(&current, || {
                        manager.textViewportLayoutController()
                    })
                    .ok()?;
                    let viewport_manager = native_markdown_table_intent_step(&current, || {
                        viewport.textLayoutManager()
                    })
                    .ok()??;
                    if !std::ptr::eq(&*viewport_manager, &*manager) {
                        return None; // 另一 manager 的 viewport 不能解释本图 clip，即使源偏移碰巧相同。
                    }
                    let range =
                        native_markdown_table_intent_step(&current, || viewport.viewportRange())
                            .ok()??;
                    let checked_source_range = |range: &NSTextRange| {
                        let source = source_range(range)?;
                        let start =
                            native_markdown_table_intent_step(&current, || range.location())
                                .ok()?;
                        let end =
                            native_markdown_table_intent_step(&current, || range.endLocation())
                                .ok()?;
                        for (location, offset) in [(&start, source.start), (&end, source.end)] {
                            // 每个真实端点都经原 provider 精确重建；单向整数偏移不能证明没有换图或外来 location。
                            let offset = isize::try_from(offset).ok()?;
                            let reconstructed = native_markdown_table_intent_step(&current, || {
                                content.locationFromLocation_withOffset(&document_start, offset)
                            })
                            .ok()??;
                            if native_markdown_table_intent_step(&current, || {
                                content
                                    .offsetFromLocation_toLocation(&document_start, &reconstructed)
                            })
                            .ok()?
                                != offset
                                || native_markdown_table_intent_step(&current, || {
                                    content.offsetFromLocation_toLocation(location, &reconstructed)
                                })
                                .ok()?
                                    != 0
                            {
                                return None; // 不 clamp、不改用新的文档起点来掩盖原端点关联失败。
                            }
                        }
                        Some(source)
                    };
                    if checked_source_range(&document_range)? != (0..document_length) {
                        return None; // viewport 的整数范围须属于原完整 provider 文档，而非另一份同长度源。
                    }
                    let viewport_source = checked_source_range(&range)?;
                    if viewport_source.is_empty() {
                        return None; // 空 viewport 没有可连续认证的正文，不能由 clip 高度猜起点。
                    }
                    let start =
                        native_markdown_table_intent_step(&current, || range.location()).ok()?;
                    let visited = Cell::new(0_usize);
                    let covered = Cell::new(None);
                    let failed = Cell::new(false);
                    let selected = RefCell::new(None);
                    let visited_lines = Cell::new(0_usize);
                    let visible_fragment_line = |fragment: &objc2_app_kit::NSTextLayoutFragment,
                                                 frame: NSRect,
                                                 element_source: &std::ops::Range<usize>,
                                                 fragment_source: &std::ops::Range<usize>|
                     -> Option<
                        Option<Retained<objc2_app_kit::NSTextLineFragment>>,
                    > {
                        // 只从 clip 顶边在本片段中的实际交集开始；巨段落的离屏前缀不逐行读取。
                        let y = visible_line_edge_offset(frame, visible_rect, origin, false)?;
                        let mut pending = native_markdown_table_intent_step(&current, || {
                            fragment.textLineFragmentForVerticalOffset_requiresExactMatch(y, false)
                        })
                        .ok()?;
                        let mut selected = None;
                        let mut previous = None;
                        while let Some(line) = pending {
                            if visited_lines.get() == MAX_VISIBLE_LINE_VIEWPORT_FRAGMENTS {
                                return None; // 复用原 viewport 有限预算，耗尽时整批失败，不截断成成功窗口。
                            }
                            visited_lines.set(visited_lines.get() + 1);
                            let bounds = native_markdown_table_intent_step(&current, || {
                                line.typographicBounds()
                            })
                            .ok()?;
                            let local_range = native_markdown_table_intent_step(&current, || {
                                line.characterRange()
                            })
                            .ok()?;
                            let source = visible_line_source_range_at_edge(
                                element_source,
                                fragment_source,
                                local_range,
                                document_length,
                                bottom,
                            )?;
                            if !visible_line_frame_is_finite(bounds)
                                || previous.is_some_and(|(end, top)| {
                                    source.start < end
                                        || (!source.is_empty() && source.end <= end)
                                        || bounds.origin.y <= top
                                })
                            {
                                return None; // 每次必须是同片段真实向后的新行；重复、倒退与非法几何没有遍历权限。
                            }
                            let line_top = origin.y + frame.origin.y + bounds.origin.y;
                            if !line_top.is_finite() {
                                return None; // 有限分量相加仍可能溢出，不以离屏分类吞掉非法几何。
                            }
                            if line_top >= visible_rect.origin.y + visible_rect.size.height {
                                break; // 只读到首个下方行即结束，不遍历段落剩余离屏内容。
                            }
                            if visible_line_intersects_clip(frame, bounds, visible_rect, origin) {
                                selected = Some(line); // 只保留一个实际行 claim，后继候选立即替换并释放前一个。
                                if !bottom {
                                    return Some(selected); // 顶边只取首个相交行；源覆盖仍由外层完整 viewport 认证。
                                }
                            }
                            if source.is_empty() || source.end == fragment_source.end {
                                break; // 真实 EOF 空行或片段末源行已结束，没有可继续的本片段后继。
                            }
                            previous = Some((source.end, bounds.origin.y));
                            let next = (bounds.origin.y + bounds.size.height).next_up();
                            if !next.is_finite() || next <= bounds.origin.y {
                                return None; // 只从原行真实排他下边界前进；溢出或无进展不能重查原行。
                            }
                            pending = native_markdown_table_intent_step(&current, || {
                                // 与现有可见行观察一致，仅跨到该排版边界后的首个浮点坐标；
                                // 不用文字位置 API 反查，避免为巨段落产生完整属性值副本。
                                fragment.textLineFragmentForVerticalOffset_requiresExactMatch(
                                    next, false,
                                )
                            })
                            .ok()?;
                        }
                        Some(selected) // nil 表示此片段的 clip 交集仅有留白，不等于整个 viewport 未布局。
                    };
                    let block = block2::RcBlock::new(
                        |pointer: NonNull<objc2_app_kit::NSTextLayoutFragment>| {
                            if !current() || visited.get() >= MAX_VISIBLE_LINE_VIEWPORT_FRAGMENTS {
                                failed.set(true);
                                return objc2::runtime::Bool::NO; // 不让下一 callback 超过固定预算或继续使用已撤销票据。
                            }
                            visited.set(visited.get() + 1);
                            let candidate = (|| -> Option<bool> {
                                // SAFETY: 公开同步枚举在 callback 内提供活 fragment；裸指针不进入缓存或后继任务。
                                let fragment = unsafe { pointer.as_ref() };
                                let state = native_markdown_table_intent_step(&current, || {
                                    fragment.state()
                                })
                                .ok()?;
                                if state
                                    != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
                                {
                                    return None; // 任何覆盖内 unknown 都拒绝整批，不读 estimated frame、跳洞或触发布局。
                                }
                                let actual_manager =
                                    native_markdown_table_intent_step(&current, || {
                                        fragment.textLayoutManager()
                                    })
                                    .ok()??;
                                let element = native_markdown_table_intent_step(&current, || {
                                    fragment.textElement()
                                })
                                .ok()??;
                                let actual_content =
                                    native_markdown_table_intent_step(&current, || {
                                        element.textContentManager()
                                    })
                                    .ok()??;
                                if !std::ptr::eq(&*actual_manager, &*manager)
                                    || !std::ptr::eq(&*actual_content, &*content)
                                {
                                    return None; // 先证明实际 graph，再通过原 provider 翻译 element/fragment 源坐标。
                                }
                                let element_range =
                                    native_markdown_table_intent_step(&current, || {
                                        element.elementRange()
                                    })
                                    .ok()??;
                                let fragment_range =
                                    native_markdown_table_intent_step(&current, || {
                                        fragment.rangeInElement()
                                    })
                                    .ok()?;
                                let element_source = checked_source_range(&element_range)?;
                                let fragment_source = checked_source_range(&fragment_range)?;
                                if fragment_source.start < element_source.start
                                    || fragment_source.end > element_source.end
                                {
                                    return None; // 真实 fragment 必须嵌套于本 element；viewport 不是修复错误来源的 envelope。
                                }
                                let frontier = visible_line_viewport_frontier(
                                    &viewport_source,
                                    covered.get(),
                                    &fragment_source,
                                )?;
                                let frame = native_markdown_table_intent_step(&current, || {
                                    fragment.layoutFragmentFrame()
                                })
                                .ok()?;
                                let position =
                                    visible_line_reverse_frame(frame, visible_rect, origin)?;
                                let needs_candidate = bottom || selected.borrow().is_none();
                                if needs_candidate
                                    && position == VisibleLineReverseFrame::Intersects
                                    && let Some(line) = visible_fragment_line(
                                        fragment,
                                        frame,
                                        &element_source,
                                        &fragment_source,
                                    )?
                                {
                                    // fragment 的段后留白也会与 clip 相交，但不一定有可见文字。
                                    // 只有真实行相交才选为边缘；空留白仍参与下方源覆盖认证，
                                    // 并继续沿同一有限 viewport 查下一片段，不请求额外布局。
                                    // slot 借用已在上一语句结束；Retained 创建及其可能的重入不携带 RefCell 借用。
                                    let retained =
                                        native_markdown_table_intent_step(&current, || {
                                            // SAFETY: callback 的 fragment 仍活，只为原 single-line/逆查验证增加一次局部 claim。
                                            unsafe { Retained::retain(pointer.as_ptr()) }
                                        })
                                        .ok()??;
                                    *selected.borrow_mut() = Some((retained, state, frame, line));
                                }
                                covered.set(Some(frontier));
                                // 即使已找到可见片段，也须证明完整 viewport 连续；不把中途 hole 或 unknown 当 Ready。
                                Some(frontier < viewport_source.end)
                            })();
                            if candidate.is_none() || !current() {
                                failed.set(true);
                                return objc2::runtime::Bool::NO;
                            }
                            candidate.unwrap_or(false).into()
                        },
                    );
                    let enumerated = native_markdown_table_intent_step(&current, || {
                        // 仅从真实 viewport.location 开始读已有片段；不从0/EOF猜位置，也不设置 EnsuresLayout/EstimatesSize。
                        manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                            Some(&start),
                            objc2_app_kit::NSTextLayoutFragmentEnumerationOptions::None,
                            &block,
                        )
                    })
                    .ok()?;
                    drop(block); // 枚举已同步返回，所有 RefCell 借用均已结束，随后才复核原 controller/range。
                    let enumerated = enumerated?;
                    let enumerated_end = usize::try_from(
                        native_markdown_table_intent_step(&current, || {
                            content.offsetFromLocation_toLocation(&document_start, &enumerated)
                        })
                        .ok()?,
                    )
                    .ok()?;
                    if failed.get()
                        || covered.get().is_none_or(|end| end < viewport_source.end)
                        || enumerated_end < viewport_source.end
                        || enumerated_end > document_length
                    {
                        return None; // 早停、枚举断裂或预算耗尽没有完整覆盖，不能仅因已取到一个真实候选而发布。
                    }
                    let actual_viewport = native_markdown_table_intent_step(&current, || {
                        manager.textViewportLayoutController()
                    })
                    .ok()?;
                    let actual_manager = native_markdown_table_intent_step(&current, || {
                        viewport.textLayoutManager()
                    })
                    .ok()??;
                    let actual_range =
                        native_markdown_table_intent_step(&current, || viewport.viewportRange())
                            .ok()??;
                    if !std::ptr::eq(&*actual_viewport, &*viewport)
                        || !std::ptr::eq(&*actual_manager, &*manager)
                        || checked_source_range(&actual_range)? != viewport_source
                    {
                        return None; // Native getter 重入改变了原 viewport 权限，不能补签新范围后继续。
                    }
                    selected.into_inner()
                })();
                // controller、range、返回 location 及临时候选均先正常析构，析构重入撤权也必须拒收。
                current().then_some(selected).flatten()
            };
            let read_edge = |bottom: bool| -> Option<VisibleLine> {
                #[cfg(feature = "native-markdown-runtime-probe")]
                {
                    diagnostic_bottom.set(bottom);
                    diagnostic_stage.set(if bottom {
                        VisibleLineReadStage::BottomFragment
                    } else {
                        VisibleLineReadStage::TopFragment
                    });
                }
                let inset = (visible_rect.size.height * 0.5).min(0.5);
                let edge = if bottom {
                    visible_rect.origin.y + visible_rect.size.height - inset
                } else {
                    visible_rect.origin.y + inset
                };
                let point = NSPoint::new(5.0, (edge - origin.y).max(0.0));
                let positioned = native_markdown_table_intent_step(&current, || {
                    manager.textLayoutFragmentForPosition(point)
                })
                .ok()?;
                visible_line_stage!(FragmentState);
                let positioned_state = match positioned.as_ref() {
                    Some(fragment) => Some(
                        native_markdown_table_intent_step(&current, || fragment.state()).ok()?,
                    ),
                    None => None, // 点落在段落外留白时没有 state，也不能读取不存在的行几何。
                };
                let reverse_seed = if bottom
                    && positioned_state
                        != Some(objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable)
                    && let Some(positioned) = positioned.as_ref()
                {
                    // 未布局候选只有源坐标可用；这里只为有限 Reverse 固定起点，不发布任何覆盖。
                    let seed_manager = native_markdown_table_intent_step(&current, || {
                        positioned.textLayoutManager()
                    })
                    .ok()??;
                    if !std::ptr::eq(&*seed_manager, &*manager) {
                        return None;
                    }
                    let range =
                        native_markdown_table_intent_step(&current, || positioned.rangeInElement())
                            .ok()?;
                    let _seed_source = source_range(&range)?; // seed 只验证原 provider 坐标，不发布源覆盖。
                    Some(native_markdown_table_intent_step(&current, || range.location()).ok()?)
                } else {
                    None // 无真实点候选时不从全文末尾创造新的逆扫起点。
                };
                let (fragment, state, prepared_frame, prepared_line) = if positioned_state
                    == Some(objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable)
                {
                    // 点候选已有真实行布局，直接沿同一对象读取，不重复请求 EOF 候选。
                    (positioned?, positioned_state?, None, None)
                } else if bottom {
                    // EOF 后留白可命中 nil 或 estimated；二者都无权提供 frame/line。
                    // 只转一次最后源 unit 的独立已有片段，后续仍须真实 state、clip 和源映射认证。
                    drop(positioned);
                    visible_line_stage!(EofFragment);
                    let offset = isize::try_from(document_length.checked_sub(1)?).ok()?;
                    let location = native_markdown_table_intent_step(&current, || {
                        content.locationFromLocation_withOffset(&document_start, offset)
                    })
                    .ok()??;
                    if native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &location)
                    })
                    .ok()?
                        != offset
                    {
                        return None; // 原最后源 unit 的位置必须精确往返，不能错用另一 Native 文档。
                    }
                    let eof_fragment = native_markdown_table_intent_step(&current, || {
                        manager.textLayoutFragmentForLocation(&location)
                    })
                    .ok()?;
                    let eof = if let Some(fragment) = eof_fragment {
                        let state =
                            native_markdown_table_intent_step(&current, || fragment.state())
                                .ok()?;
                        if state == objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable {
                            let frame = native_markdown_table_intent_step(&current, || {
                                fragment.layoutFragmentFrame()
                            })
                            .ok()?;
                            visible_line_edge_offset(frame, visible_rect, origin, true)
                                .map(|_| (fragment, state, frame))
                        } else {
                            None // EOF 候选未布局时不查询其 frame，也不给它任何行或源覆盖权限。
                        }
                    } else {
                        None // 未缓存 EOF 片段不等于没有可见正文，仍可沿原真实 seed 有限逆查。
                    };
                    if let Some((fragment, state, frame)) = eof {
                        // 真正 EOF 片段已在 clip 内，直接验证它的末行，不再枚举。
                        (fragment, state, Some(frame), None)
                    } else {
                        // EOF 临时片段已释放；只从原点候选之前作一次有限逆扫，不从全文末尾起扫。
                        let seed = reverse_seed?;
                        let (fragment, state, frame) = reverse_candidate(&seed)?;
                        (fragment, state, Some(frame), None)
                    }
                } else {
                    // 顶边点接口的 nil/placeholder 不代表真实已绘 viewport 没有行；只改用其原连续已布局片段选候选。
                    drop(positioned);
                    let (fragment, state, frame, line) = viewport_edge_candidate(false)?;
                    (fragment, state, Some(frame), Some(line)) // 源权限仍由后续真实行/来源/元数据/最终几何验证赋予。
                };
                visible_line_stage!(FragmentState);
                if state != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable {
                    // 一次 EOF 候选也未布局就结束；不重查原 estimated、ensure 或退回 paragraph。
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_visible_line_edge_geometry(
                        false,
                        state.0,
                        None,
                        visible_rect,
                        origin,
                        mode == MarkdownPresentationMode::Source,
                        bottom,
                    );
                    return None;
                }
                visible_line_stage!(FragmentFrame);
                let frame = match prepared_frame {
                    Some(frame) => frame, // EOF 或 Reverse 已读取真实 frame，后续统一作完整几何复核。
                    None => native_markdown_table_intent_step(&current, || {
                        fragment.layoutFragmentFrame()
                    })
                    .ok()?,
                };
                let Some(y) = visible_line_edge_offset(frame, visible_rect, origin, bottom) else {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_visible_line_edge_geometry(
                        true,
                        state.0,
                        Some(frame),
                        visible_rect,
                        origin,
                        mode == MarkdownPresentationMode::Source,
                        bottom,
                    );
                    return None; // 诊断只观察原失败标量，不扩大 clip 或补取未经布局的 frame。
                };
                visible_line_stage!(Line);
                #[cfg(feature = "native-markdown-runtime-probe")]
                let trace_line_failure = |reason: &'static str, observed_state: Option<usize>| {
                    use std::sync::atomic::{AtomicUsize, Ordering};
                    static COUNT: AtomicUsize = AtomicUsize::new(0);
                    if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
                        && COUNT.fetch_add(1, Ordering::Relaxed) < 12
                    {
                        // 只区分原 getter 的撤权、state 变化与真实 nil；不为诊断重读 Native 或续签原票。
                        eprintln!(
                            "native_visible_line_query_rejected reason={reason} selected_state={} observed_state={observed_state:?} y={y} frame={frame:?} clip={visible_rect:?} origin={origin:?} source={} bottom={bottom} formal=false",
                            state.0,
                            mode == MarkdownPresentationMode::Source,
                        );
                    }
                };
                let line_state =
                    match native_markdown_table_intent_step(&current, || fragment.state()) {
                        Ok(state) => state,
                        Err(_) => {
                            #[cfg(feature = "native-markdown-runtime-probe")]
                            trace_line_failure("state_revoked", None);
                            return None; // state getter 前后原权限不再成立，不能以旧 state 继续取行。
                        }
                    };
                if line_state != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_line_failure("state_changed", Some(line_state.0));
                    return None; // 选择和临时 claim 析构之后仍须有实际布局，才可调用单行 getter。
                }
                let prepared_by_viewport = prepared_line.is_some();
                let line = match prepared_line {
                    Some(line) => Some(line), // 原 viewport 已取得真实相交行，不能丢弃它再向留白坐标重查。
                    None => match native_markdown_table_intent_step(&current, || {
                        fragment.textLineFragmentForVerticalOffset_requiresExactMatch(y, false)
                    }) {
                        Ok(line) => line,
                        Err(_) => {
                            #[cfg(feature = "native-markdown-runtime-probe")]
                            trace_line_failure("line_revoked", Some(line_state.0));
                            return None; // 原 getter 重入撤权与实际没有匹配行分别保留，不靠重新读取掩盖。
                        }
                    },
                };
                #[cfg(feature = "native-markdown-runtime-probe")]
                if line.is_none() {
                    trace_line_failure("line_nil", Some(line_state.0));
                }
                let (fragment, frame, line, from_viewport) = if let Some(line) = line {
                    (fragment, frame, line, prepared_by_viewport)
                } else {
                    // 点接口可命中段后留白；nil 行不能把整个可见区误判为未准备。
                    // 只读取已布局、源覆盖连续的原 viewport，取其首个或末个实际可见行；
                    // 直接保留候选行，不在相同留白坐标重查，也不用片段源末端猜最后一行。
                    drop(fragment);
                    let (fragment, _, frame, line) = viewport_edge_candidate(bottom)?;
                    (fragment, frame, line, true)
                };
                visible_line_stage!(LineGeometry);
                let line_bounds =
                    native_markdown_table_intent_step(&current, || line.typographicBounds())
                        .ok()?;
                let local_range =
                    native_markdown_table_intent_step(&current, || line.characterRange()).ok()?;
                if !visible_line_frame_is_finite(line_bounds) {
                    return None; // 非法行几何仍直接拒绝，不能把它伪装成合法留白后换另一个候选。
                }
                let (fragment, frame, line, line_bounds, local_range) =
                    if visible_line_intersects_clip(frame, line_bounds, visible_rect, origin) {
                        (fragment, frame, line, line_bounds, local_range)
                    } else {
                        if from_viewport {
                            return None; // 已选原 viewport 行随后变为离屏就是失败，不再次枚举续接新的几何。
                        }
                        #[cfg(feature = "native-markdown-runtime-probe")]
                        {
                            use std::sync::atomic::{AtomicUsize, Ordering};
                            static COUNT: AtomicUsize = AtomicUsize::new(0);
                            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some()
                                && COUNT.fetch_add(1, Ordering::Relaxed) < 8
                            {
                                // 原 getter 均成功后仅记录已有值；不为诊断补布局、取正文或重新捕获票据。
                                eprintln!(
                                    "native_visible_line_beyond_clip frame={frame:?} line_bounds={line_bounds:?} local_range={local_range:?} clip={visible_rect:?} origin={origin:?} source={} bottom={bottom} formal=false",
                                    mode == MarkdownPresentationMode::Source,
                                );
                            }
                        }
                        // 非精确查询也可能返回 clip 下方下一行；只归还这份合法离屏候选，
                        // 沿同一原 viewport 的连续覆盖取得真正相交的边缘行，不授予离屏行可见权限。
                        drop(line);
                        drop(fragment);
                        let (fragment, _, frame, line) = viewport_edge_candidate(bottom)?;
                        let line_bounds = native_markdown_table_intent_step(&current, || {
                            line.typographicBounds()
                        })
                        .ok()?;
                        let local_range =
                            native_markdown_table_intent_step(&current, || line.characterRange())
                                .ok()?;
                        if !visible_line_intersects_clip(frame, line_bounds, visible_rect, origin) {
                            return None; // 候选读取后已不相交则仍失败，不再选新候选或把旧几何贴回当前行。
                        }
                        (fragment, frame, line, line_bounds, local_range)
                    };
                visible_line_stage!(Element);
                let element =
                    native_markdown_table_intent_step(&current, || fragment.textElement())
                        .ok()??;
                visible_line_stage!(ElementAssociation);
                let actual_manager =
                    native_markdown_table_intent_step(&current, || fragment.textLayoutManager())
                        .ok()??;
                let actual_content =
                    native_markdown_table_intent_step(&current, || element.textContentManager())
                        .ok()??;
                if !std::ptr::eq(&*actual_manager, &*manager)
                    || !std::ptr::eq(&*actual_content, &*content)
                {
                    // 解释局部 location 前即拒绝其他图的对象，不借当前 provider 转译外来坐标。
                    return None;
                }
                visible_line_stage!(ElementRange);
                let element_range =
                    native_markdown_table_intent_step(&current, || element.elementRange())
                        .ok()??;
                let fragment_range =
                    native_markdown_table_intent_step(&current, || fragment.rangeInElement())
                        .ok()?;
                let element_source = source_range(&element_range)?;
                let fragment_source = source_range(&fragment_range)?;
                visible_line_stage!(SourceMap);
                let source = visible_line_source_range_at_edge(
                    &element_source,
                    &fragment_source,
                    local_range,
                    document_length,
                    bottom,
                );
                #[cfg(feature = "native-markdown-runtime-probe")]
                let source_mapping = source.as_ref().map(|range| (range.start, range.end));
                let Some(source) = source else {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    trace_visible_line_source_map(
                        VisibleLineSourceMapDiagnostic {
                            element_source: (element_source.start, element_source.end),
                            fragment_source: (fragment_source.start, fragment_source.end),
                            local_range,
                            line_bounds,
                            frame,
                            document_length,
                            mapped: source_mapping,
                        },
                        mode == MarkdownPresentationMode::Source,
                        bottom,
                    );
                    return None; // 只记录原映射失败；不重查 Native、不移动范围或扩大语义权限。
                };
                visible_line_stage!(Inverse);
                if source.is_empty() {
                    // EOF 空视觉行不包含源字符，排他 endLocation 没有普通 downstream 字符行可逆查。
                    // 只沿原 provider 的真实 document end 验证同一终点，不改 upstream 指向上一行。
                    let end = native_markdown_table_intent_step(&current, || {
                        document_range.endLocation()
                    })
                    .ok()?;
                    let end_offset = native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &end)
                    })
                    .ok()?;
                    let reconstructed = native_markdown_table_intent_step(&current, || {
                        content.locationFromLocation_withOffset(&document_start, end_offset)
                    })
                    .ok()??;
                    let reconstructed_offset = native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &reconstructed)
                    })
                    .ok()?;
                    let difference = native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&end, &reconstructed)
                    })
                    .ok()?;
                    if !visible_line_eof_location_round_trip(
                        document_length,
                        end_offset,
                        reconstructed_offset,
                        difference,
                    ) {
                        // 原 document end、重建位置与 canonical EOF 必须完全一致，不能容差或 clamp。
                        return None;
                    }
                } else {
                    // 原单行已经由真实几何选出；只认证其原属性值坐标与精确竖向成员关系。
                    super::line_source_metadata::verify_native_markdown_line_metadata(
                        super::line_source_metadata::NativeMarkdownLineMetadataRequest {
                            fragment: &fragment,
                            line: &line,
                            content: &content,
                            document_start: &document_start,
                            document_length,
                            element_source: &element_source,
                            source: &source,
                            local_range,
                            bounds: line_bounds,
                        },
                        &current,
                    )
                    .ok()?;
                }
                Some(VisibleLine {
                    fragment,
                    element,
                    line,
                    frame,
                    line_bounds,
                    local_range,
                    element_source,
                    fragment_source,
                    source,
                })
            };
            let first = read_edge(false)?;
            let last = read_edge(true)?;
            visible_line_stage!(Verify);
            for observed in [&first, &last] {
                // 第二个边缘 getter 可重入重排第一个；发布前复核原对象、范围、frame 和 manager。
                let actual_manager = native_markdown_table_intent_step(&current, || {
                    observed.fragment.textLayoutManager()
                })
                .ok()??;
                let actual_content = native_markdown_table_intent_step(&current, || {
                    observed.element.textContentManager()
                })
                .ok()??;
                let actual_element =
                    native_markdown_table_intent_step(&current, || observed.fragment.textElement())
                        .ok()??;
                let element_range =
                    native_markdown_table_intent_step(&current, || observed.element.elementRange())
                        .ok()??;
                let fragment_range = native_markdown_table_intent_step(&current, || {
                    observed.fragment.rangeInElement()
                })
                .ok()?;
                if !std::ptr::eq(&*actual_manager, &*manager)
                    || !std::ptr::eq(&*actual_content, &*content)
                    || !std::ptr::eq(&*actual_element, &*observed.element)
                    || source_range(&element_range)? != observed.element_source
                    || source_range(&fragment_range)? != observed.fragment_source
                    || native_markdown_table_intent_step(&current, || {
                        observed.fragment.layoutFragmentFrame()
                    })
                    .ok()?
                        != observed.frame
                    || native_markdown_table_intent_step(&current, || observed.fragment.state())
                        .ok()?
                        != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
                    || native_markdown_table_intent_step(&current, || {
                        observed.line.characterRange()
                    })
                    .ok()?
                        != observed.local_range
                    || native_markdown_table_intent_step(&current, || {
                        observed.line.typographicBounds()
                    })
                    .ok()?
                        != observed.line_bounds
                {
                    // 任一变化丢弃整批；不能以两个不同布局时刻的行拼出一个看似有效的窗口。
                    return None;
                }
            }
            visible_line_stage!(Window);
            let window = visible_line_source_window(&first.source, &last.source, document_length)?;
            {
                let document = identity.upgrade()?;
                let document = document.borrow();
                visible_line_stage!(Scalar);
                // line 是系统换行结果，仍核实 UTF-16 两端未切断 canonical Unicode scalar。
                document.byte_offset_for_utf16(window.start).ok()?;
                document.byte_offset_for_utf16(window.end).ok()?;
            }
            visible_line_stage!(FinalGeometry);
            if native_markdown_table_intent_step(&current, || self.textContainerOrigin()).ok()?
                != origin
                || native_markdown_table_intent_step(&current, || self.visibleRect()).ok()?
                    != visible_rect
                || native_markdown_table_intent_step(&current, || self.bounds()).ok()? != bounds
            {
                // 几何在只读查询间也可能改变；旧 clip 或列宽不能用于当前语义和命中。
                return None;
            }
            visible_line_stage!(FinalGraph);
            let final_manager =
                native_markdown_table_intent_step(&current, || self.textLayoutManager()).ok()??;
            let final_content =
                native_markdown_table_intent_step(&current, || manager.textContentManager())
                    .ok()??;
            if !std::ptr::eq(&*final_manager, &*manager)
                || !std::ptr::eq(&*final_content, &*content)
            {
                // manager/container 在重入中换图，即使原对象仍被局部 retain 也不能继续使用。
                return None;
            }
            visible_line_stage!(FinalAuthority);
            current().then_some(window)
        })(); // 所有临时 line/fragment/element/manager claims 在最终纯权限检查前结束。
        #[cfg(feature = "native-markdown-runtime-probe")]
        if result.is_none() {
            // 只记录原退出前已经执行的固定阶段；不为日志重做 Native getter 或读取正文。
            trace_visible_line_read_failure(
                diagnostic_stage.get(),
                diagnostic_source.get(),
                diagnostic_bottom.get(),
            );
        }
        if capture.current()
            && self.ivars().table_input_intent_epoch.get() == input_epoch
            && !self.ivars().textkit_graph_teardown.get()
        {
            result.unwrap_or(0..0) // 缺真实行代表 Pending，不升级为完整 paragraph 或全文语义。
        } else {
            #[cfg(feature = "native-markdown-runtime-probe")]
            trace_visible_line_read_failure(
                VisibleLineReadStage::FinalAuthority,
                diagnostic_source.get(),
                diagnostic_bottom.get(),
            );
            0..0 // Native getter 或局部析构撤销了原票据，旧范围不进入后继布局批次。
        }
    }
}

/// 只给显式 probe 的首个失败分类，不改变默认类布局、读范围或重试策略。
#[cfg(feature = "native-markdown-runtime-probe")]
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum VisibleLineReadStage {
    Admission,
    Owner,
    Storage,
    Document,
    Manager,
    ExpectedManager,
    ManagerAssociation,
    Content,
    Editing,
    Geometry,
    Viewport,
    Provider,
    TopFragment,
    BottomFragment,
    EofFragment,
    FragmentState,
    FragmentFrame,
    Line,
    LineGeometry,
    Element,
    ElementAssociation,
    ElementRange,
    SourceMap,
    Inverse,
    Verify,
    Window,
    Scalar,
    FinalGeometry,
    FinalGraph,
    FinalAuthority,
}

/// 每个固定阶段、展示模式最多输出一次；总共至多 60 条，无正文、路径或地址。
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_visible_line_read_failure(stage: VisibleLineReadStage, source: bool, bottom: bool) {
    use std::sync::atomic::{AtomicU64, Ordering};
    if super::viewport_reentry_diagnostic::source_anchor_active() {
        static ANCHOR_LOGS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        if ANCHOR_LOGS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < 4).then_some(count + 1)
            })
            .is_ok()
        {
            // 启动阶段的同类Pending不能遮住真实模式动作的失败；仅记录原出口已有分类。
            eprintln!(
                "native_visible_line_anchor_pending stage={stage:?} source={source} bottom={bottom} formal=false"
            );
        }
    }
    static SEEN: AtomicU64 = AtomicU64::new(0);
    let bit = 1_u64 << (u32::from(stage as u8) * 2 + u32::from(source));
    if SEEN.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return; // 重复 Pending 不扩张日志，也不追加布局或查询工作。
    }
    eprintln!("native_visible_line_pending stage={stage:?} source={source} bottom={bottom}");
}

/// 两种失败、两种模式及两个边缘各观察一次；只输出原读取已取得的 state/frame/clip/origin。
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_visible_line_edge_geometry(
    frame_checked: bool,
    state: usize,
    frame: Option<NSRect>,
    clip: NSRect,
    origin: NSPoint,
    source: bool,
    bottom: bool,
) {
    use std::sync::atomic::{AtomicU8, Ordering};
    static SEEN: AtomicU8 = AtomicU8::new(0);
    let bit = 1_u8 << (u32::from(frame_checked) * 4 + u32::from(source) * 2 + u32::from(bottom));
    if SEEN.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return; // 总共至多八条；重复的 Pending 不压过随后 Source 或另一个边缘的真实证据。
    }
    eprintln!(
        "native_visible_line_edge frame_checked={frame_checked} state={state} frame={frame:?} clip={clip:?} origin={origin:?} source={source} bottom={bottom}"
    );
}

/// 映射失败时已在同步栈中的纯值，不持 Native 对象、正文或文档身份。
#[cfg(feature = "native-markdown-runtime-probe")]
struct VisibleLineSourceMapDiagnostic {
    element_source: (usize, usize),
    fragment_source: (usize, usize),
    local_range: NSRange,
    line_bounds: NSRect,
    frame: NSRect,
    document_length: usize,
    mapped: Option<(usize, usize)>,
}

/// 每种模式及边缘最多输出一次原映射失败，四条上限不会随滚动或 getter 重入增长。
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_visible_line_source_map(
    observed: VisibleLineSourceMapDiagnostic,
    source: bool,
    bottom: bool,
) {
    use std::sync::atomic::{AtomicU8, Ordering};
    static SEEN: AtomicU8 = AtomicU8::new(0);
    let bit = 1_u8 << (u32::from(source) * 2 + u32::from(bottom));
    if SEEN.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return; // 只保留首个真实反例；诊断不读取额外行、源码或复制正文。
    }
    eprintln!(
        "native_visible_line_source_map element={:?} fragment={:?} local={:?} line_bounds={:?} frame={:?} length={} mapped={:?} source={source} bottom={bottom}",
        observed.element_source,
        observed.fragment_source,
        observed.local_range,
        observed.line_bounds,
        observed.frame,
        observed.document_length,
        observed.mapped,
    );
}

/// 每种模式的命中/未命中各记录一次，只观察本次有限枚举已取得的计数和结果。
#[cfg(feature = "native-markdown-runtime-probe")]
fn trace_visible_line_reverse(visited: usize, matched: bool, source: bool) {
    use std::sync::atomic::{AtomicU8, Ordering};
    static SEEN: AtomicU8 = AtomicU8::new(0);
    let bit = 1_u8 << (u32::from(source) * 2 + u32::from(matched));
    if SEEN.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return; // 最多四条，不在诊断中重做 Native 查询或读取源范围。
    }
    eprintln!(
        "native_visible_line_reverse visited={visited} matched={matched} source={source} limit=128"
    );
}

/// 空行的已布局 fragment 可以没有字形宽度，仍须有有限且为正的真实排版高度。
fn visible_line_frame_is_finite(frame: NSRect) -> bool {
    [
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
        frame.origin.x + frame.size.width,
        frame.origin.y + frame.size.height,
    ]
    .into_iter()
    .all(f64::is_finite)
        && frame.size.width >= 0.0
        && frame.size.height > 0.0
}

/// Reverse 只按真实垂直位置选择候选；这三个结果都不携带任何源覆盖权限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisibleLineReverseFrame {
    Above,
    Below,
    Intersects,
}

/// 仅判断已布局 frame 与 clip；越过顶边即停止，未相交的 EOF 不能成为可见末端。
fn visible_line_reverse_frame(
    frame: NSRect,
    clip: NSRect,
    origin: NSPoint,
) -> Option<VisibleLineReverseFrame> {
    if !visible_line_frame_is_finite(frame)
        || !mode_rect_is_finite(clip)
        || !origin.x.is_finite()
        || !origin.y.is_finite()
    {
        return None; // 非有限、零高或非法 clip 使本批失败，不能伪装成略过一个离屏片段。
    }
    let bottom = frame.origin.y + frame.size.height + origin.y;
    if !bottom.is_finite() {
        return None; // 分量分别有限仍可能求和溢出，没有真实可比较的垂直范围。
    }
    if bottom <= clip.origin.y {
        return Some(VisibleLineReverseFrame::Above);
    }
    if visible_line_edge_offset(frame, clip, origin, true).is_some() {
        Some(VisibleLineReverseFrame::Intersects)
    } else {
        Some(VisibleLineReverseFrame::Below)
    }
}

/// viewport 只限定只读枚举入口；每一步必须由真实 fragment 精确连续推进，不能用 envelope 填洞。
fn visible_line_viewport_frontier(
    viewport: &std::ops::Range<usize>,
    covered: Option<usize>,
    fragment: &std::ops::Range<usize>,
) -> Option<usize> {
    if viewport.start >= viewport.end || fragment.start >= fragment.end {
        return None; // 空 viewport 或零源 fragment 没有向前覆盖能力，不能制造可见正文。
    }
    match covered {
        None if fragment.start <= viewport.start && viewport.start < fragment.end => {
            Some(fragment.end) // 首片可以从 viewport 前开始，但必须真正包含其首源位置。
        }
        Some(end) if end < viewport.end && fragment.start == end => {
            Some(fragment.end) // 后续必须紧接原终点；gap、重叠、重复和倒退都不能视为连续。
        }
        _ => None, // 已覆盖终点以后无需新增候选，或当前来源不再能证明原窗口的连续性。
    }
}

/// 边缘采样位于真实 fragment/clip 交集内；只选择查询坐标，不由像素反推字符数量。
fn visible_line_edge_offset(
    frame: NSRect,
    clip: NSRect,
    origin: NSPoint,
    bottom: bool,
) -> Option<f64> {
    if !visible_line_frame_is_finite(frame)
        || !mode_rect_is_finite(clip)
        || !origin.x.is_finite()
        || !origin.y.is_finite()
    {
        // clip 必须有实际面积；fragment 只允许空行的零宽，负宽、零高和非有限坐标仍无权限。
        return None;
    }
    let top = frame.origin.y + origin.y;
    let from = top.max(clip.origin.y);
    let to = (top + frame.size.height).min(clip.origin.y + clip.size.height);
    if ![top, from, to].into_iter().all(f64::is_finite) || from >= to {
        // 只允许实际相交片段；paragraph 附近的离屏 fragment 不参与可见范围。
        return None;
    }
    let inset = ((to - from) * 0.5).min(0.5);
    Some(if bottom { to - inset } else { from + inset } - top)
}

/// 最近行可能位于 paragraph 的留白外侧；只接收确实与垂直 clip 相交的排版行。
fn visible_line_intersects_clip(
    frame: NSRect,
    line: NSRect,
    clip: NSRect,
    origin: NSPoint,
) -> bool {
    if !visible_line_frame_is_finite(frame)
        || !mode_rect_is_finite(clip)
        || !origin.x.is_finite()
        || !origin.y.is_finite()
    {
        // 零宽空行按垂直排版范围判断；非法 frame/clip 仍独立拒绝，不依赖调用顺序。
        return false;
    }
    let top = origin.y + frame.origin.y + line.origin.y;
    let bottom = top + line.size.height;
    [top, bottom, line.origin.x, line.size.width]
        .into_iter()
        .all(f64::is_finite)
        && line.size.width >= 0.0
        && line.size.height > 0.0
        && top < clip.origin.y + clip.size.height
        && clip.origin.y < bottom
}

/// 复用 element 原点与 fragment 嵌套规则，保留真实整行范围而非整段 envelope。
fn visible_line_source_range(
    element: &std::ops::Range<usize>,
    fragment: &std::ops::Range<usize>,
    line: NSRange,
    document_length: usize,
) -> Option<std::ops::Range<usize>> {
    if element.end > document_length {
        // 同一局部行值不能被映射到已缩短正文之外。
        return None;
    }
    let start = mode_line_source_candidate(element, fragment, line)?;
    Some(start..start.checked_add(line.length)?)
}

/// 可见 EOF 空行只给底边提供终点，不把任意零长视觉行当作已经覆盖的正文。
pub(in crate::native_surface) fn visible_line_source_range_at_edge(
    element: &std::ops::Range<usize>,
    fragment: &std::ops::Range<usize>,
    line: NSRange,
    document_length: usize,
    bottom: bool,
) -> Option<std::ops::Range<usize>> {
    if line.length != 0 {
        // 普通源行继续完整复用原 element 原点、fragment 嵌套及长度规则。
        return visible_line_source_range(element, fragment, line, document_length);
    }
    if !bottom
        || element.start > fragment.start
        || fragment.start > fragment.end
        || fragment.end != document_length
        || element.end != document_length
        || element.start.checked_add(line.location)? != document_length
    {
        // 只有原 element/fragment 的真实 EOF 尾端可授权零长终点；中途空行和顶边均不扩张范围。
        return None;
    }
    Some(document_length..document_length)
}

/// 真正 EOF 以 document end 和重建位置的精确往返认证；不把排他终点当作被某行包含的字符。
fn visible_line_eof_location_round_trip(
    document_length: usize,
    original_offset: isize,
    reconstructed_offset: isize,
    difference: isize,
) -> bool {
    let Ok(expected) = isize::try_from(document_length) else {
        // Native offset 无法表示的文档长度没有合法终点，不以整数截断继续定位。
        return false;
    };
    original_offset == expected && reconstructed_offset == expected && difference == 0
}

/// 两边可以是同一源行，底边也可为真实 EOF 终点；空终点本身永远不生成任何正文。
fn visible_line_source_window(
    first: &std::ops::Range<usize>,
    last: &std::ops::Range<usize>,
    document_length: usize,
) -> Option<std::ops::Range<usize>> {
    if first.start >= first.end
        || first.end > document_length
        || last.start > last.end
        || last.end > document_length
    {
        // 必须先有顶边的真实非空源行；单独的 EOF caret 或非法范围不产生字符覆盖。
        return None;
    }
    if last.is_empty() {
        // 零长底边只允许已证明的文档末端，不能把中间 gap 的一个 caret 伪装为可见末行。
        return (last.start == document_length).then_some(first.start..document_length);
    }
    // 普通两行仍必须按源顺序且不相互部分重叠，拒绝拼接不同布局时刻的坐标。
    (first == last || first.end <= last.start).then_some(first.start..last.end)
}

#[cfg(test)]
mod visible_line_window_tests {
    use super::*;

    #[test]
    fn viewport_candidate_coverage_requires_actual_contiguous_fragment_ranges() {
        let viewport = 1_000..1_600;
        // 原 viewport 可以落在首个真实 fragment 内，但只读范围必须随后完整连续覆盖其尾端。
        let first = visible_line_viewport_frontier(&viewport, None, &(980..1_120)).unwrap();
        let second =
            visible_line_viewport_frontier(&viewport, Some(first), &(1_120..1_400)).unwrap();
        let last =
            visible_line_viewport_frontier(&viewport, Some(second), &(1_400..1_680)).unwrap();
        assert_eq!((first, second, last), (1_120, 1_400, 1_680));
        assert!(last >= viewport.end);
        // 只接一个真实可见片段仍没有覆盖权限；中部 gap、重叠、重复和倒退均不能靠 envelope 补齐。
        assert!(second < viewport.end);
        for fragment in [1_401..1_680, 1_399..1_680, 1_120..1_400, 800..900] {
            assert!(visible_line_viewport_frontier(&viewport, Some(second), &fragment).is_none());
        }
        assert!(visible_line_viewport_frontier(&viewport, Some(last), &(1_680..1_700)).is_none());
    }

    #[test]
    fn viewport_candidate_rejects_empty_and_noncontaining_initial_fragments() {
        // 空范围不能推进源覆盖；恰触及 viewport 起点但不包含它也不是首片。
        for fragment in [
            900..1_000,
            1_001..1_200,
            1_000..1_000,
            std::ops::Range {
                start: 1_100,
                end: 1_000,
            },
        ] {
            assert!(visible_line_viewport_frontier(&(1_000..1_600), None, &fragment).is_none());
        }
        assert!(visible_line_viewport_frontier(&(1_000..1_000), None, &(900..1_100)).is_none());
        assert_eq!(
            visible_line_viewport_frontier(&(1_000..1_600), None, &(900..1_700)),
            Some(1_700)
        );
        // 源端点只做整数比较，不以加法制造溢出或把真实大偏移截成固定字符预算。
        assert_eq!(
            visible_line_viewport_frontier(
                &(usize::MAX - 2..usize::MAX),
                None,
                &(usize::MAX - 3..usize::MAX),
            ),
            Some(usize::MAX)
        );
    }

    #[test]
    fn reverse_selection_skips_the_real_offscreen_eof_and_stops_above_the_clip() {
        let clip = NSRect::new(NSPoint::new(-74.0, 6_819.5), NSSize::new(720.0, 520.0));
        let origin = NSPoint::new(36.0, 32.0);
        let observed_eof = NSRect::new(
            NSPoint::new(5.0, 7_320.761_467_889_908),
            NSSize::new(551.683, 51.0),
        );
        // 实测 EOF 上边7352.76在clip底7339.5之外，不能凭“最后源字符”授予可见性。
        assert_eq!(
            visible_line_reverse_frame(observed_eof, clip, origin),
            Some(VisibleLineReverseFrame::Below)
        );
        let previous = NSRect::new(NSPoint::new(5.0, 7_250.0), NSSize::new(550.0, 51.0));
        assert_eq!(
            visible_line_reverse_frame(previous, clip, origin),
            Some(VisibleLineReverseFrame::Intersects)
        );
        let before_clip = NSRect::new(NSPoint::new(5.0, 6_750.0), NSSize::new(0.0, 17.0));
        assert_eq!(
            visible_line_reverse_frame(before_clip, clip, origin),
            Some(VisibleLineReverseFrame::Above)
        );
        assert!(
            visible_line_reverse_frame(
                NSRect::new(previous.origin, NSSize::new(550.0, 0.0)),
                clip,
                origin,
            )
            .is_none()
        );
        assert!(visible_line_reverse_frame(previous, clip, NSPoint::new(36.0, f64::NAN)).is_none());
    }

    #[test]
    fn eof_location_round_trip_rejects_stale_or_foreign_native_endpoints() {
        assert!(visible_line_eof_location_round_trip(
            10_683, 10_683, 10_683, 0
        ));
        for (original, reconstructed, difference) in [
            (10_682, 10_683, 0),
            (10_683, 10_684, 0),
            (10_683, 10_683, 1),
            (10_683, 10_683, -1),
            (-1, -1, 0),
        ] {
            // 即使局部视觉行与旧 frame 相同，过期终点或不同 Native location 也不能认证当前 EOF。
            assert!(!visible_line_eof_location_round_trip(
                10_683,
                original,
                reconstructed,
                difference,
            ));
        }
        assert!(!visible_line_eof_location_round_trip(usize::MAX, 0, 0, 0));
        let endpoint = 10_683..10_683;
        assert!(visible_line_source_window(&endpoint, &endpoint, 10_683).is_none());
    }

    #[test]
    fn real_visible_eof_visual_line_contributes_only_the_verified_bottom_endpoint() {
        let element = 10_599..10_683;
        let fragment = element.clone();
        let line = NSRange::new(84, 0);
        let endpoint =
            visible_line_source_range_at_edge(&element, &fragment, line, 10_683, true).unwrap();
        // 实测零长视觉行位于 element 局部84，只有 EOF10683 这个端点，没有新的源 unit。
        assert_eq!(endpoint, 10_683..10_683);
        assert_eq!(
            visible_line_source_window(&(10_620..10_642), &endpoint, 10_683),
            Some(10_620..10_683)
        );
        assert!(visible_line_source_window(&endpoint, &endpoint, 10_683).is_none());
        assert!(
            visible_line_source_range_at_edge(&element, &fragment, line, 10_683, false).is_none()
        );
        assert!(
            visible_line_source_range_at_edge(
                &element,
                &fragment,
                NSRange::new(83, 0),
                10_683,
                true
            )
            .is_none()
        );
        assert!(
            visible_line_source_range_at_edge(&element, &(10_599..10_682), line, 10_683, true)
                .is_none()
        );
        assert!(
            visible_line_source_range_at_edge(&(10_599..10_684), &fragment, line, 10_683, true)
                .is_none()
        );
        assert!(visible_line_source_window(&(10_620..10_642), &(10_650..10_650), 10_683).is_none());
        assert!(visible_line_source_window(&(10_683..10_684), &endpoint, 10_683).is_none());
        // 几何仍来自实测的54点 fragment和末端零宽行，EOF授权没有替代真实垂直相交。
        let frame = NSRect::new(
            NSPoint::new(5.0, 6_609.724_726_562_499),
            NSSize::new(637.687_933_183_365_9, 54.0),
        );
        let bounds = NSRect::new(NSPoint::new(0.0, 36.0), NSSize::new(0.0, 18.0));
        let origin = NSPoint::new(36.0, 32.0);
        let clip = NSRect::new(NSPoint::new(0.0, 6_650.0), NSSize::new(720.0, 520.0));
        assert!(visible_line_intersects_clip(frame, bounds, clip, origin));
        assert!(!visible_line_intersects_clip(
            frame,
            bounds,
            NSRect::new(NSPoint::new(0.0, 7_000.0), clip.size),
            origin,
        ));
    }

    #[test]
    fn laid_out_zero_width_empty_lines_keep_their_real_source_and_vertical_clip() {
        let origin = NSPoint::new(36.0, 32.0);
        let preview_clip = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(720.0, 520.0));
        let preview = NSRect::new(NSPoint::new(5.0, 474.484_726_562_5), NSSize::new(0.0, 18.0));
        let preview_y = visible_line_edge_offset(preview, preview_clip, origin, true).unwrap();
        assert!((preview_y - 13.015_273_437_5).abs() < 1e-9);
        assert!(visible_line_intersects_clip(
            preview,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 18.0)),
            preview_clip,
            origin,
        ));
        // Source 的 ruler 产生负 clip x；空行仍由原生垂直几何和真实 LF 源范围授权。
        let source_clip = NSRect::new(NSPoint::new(-74.0, 52.36), NSSize::new(720.0, 520.0));
        let source = NSRect::new(NSPoint::new(5.0, 17.0), NSSize::new(0.0, 17.0));
        let source_y = visible_line_edge_offset(source, source_clip, origin, false).unwrap();
        assert!((source_y - 3.86).abs() < 1e-9);
        assert!(visible_line_intersects_clip(
            source,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 17.0)),
            source_clip,
            origin,
        ));
        let source_range =
            visible_line_source_range(&(100..101), &(100..101), NSRange::new(0, 1), 101).unwrap();
        assert_eq!(source_range, 100..101);
        assert_eq!(
            visible_line_source_window(&source_range, &source_range, 101),
            Some(100..101)
        );
        // 没有任何源 unit 的 EOF 空行不能被零宽几何升级为虚构正文。
        assert!(
            visible_line_source_range(&(101..101), &(101..101), NSRange::new(0, 0), 101).is_none()
        );
    }

    #[test]
    fn zero_width_admission_keeps_invalid_frame_and_nonintersecting_line_rejections() {
        let clip = NSRect::new(NSPoint::new(-74.0, 52.36), NSSize::new(720.0, 520.0));
        let origin = NSPoint::new(36.0, 32.0);
        let valid = NSRect::new(NSPoint::new(5.0, 17.0), NSSize::new(0.0, 17.0));
        for invalid in [
            NSRect::new(valid.origin, NSSize::new(-0.5, 17.0)),
            NSRect::new(valid.origin, NSSize::new(0.0, 0.0)),
            NSRect::new(valid.origin, NSSize::new(0.0, f64::NAN)),
            NSRect::new(NSPoint::new(f64::INFINITY, 17.0), valid.size),
            NSRect::new(NSPoint::new(5.0, f64::MAX), NSSize::new(0.0, f64::MAX)),
        ] {
            // 只修复实际空行的宽度条件，不能把负宽、零高或坐标溢出带入 Native 行查询。
            assert!(visible_line_edge_offset(invalid, clip, origin, false).is_none());
            assert!(!visible_line_intersects_clip(invalid, valid, clip, origin));
        }
        let offscreen = NSRect::new(NSPoint::new(5.0, 0.0), NSSize::new(0.0, 17.0));
        assert!(visible_line_edge_offset(offscreen, clip, origin, false).is_none());
        assert!(!visible_line_intersects_clip(
            offscreen,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 17.0)),
            clip,
            origin,
        ));
    }

    #[test]
    fn giant_fragment_uses_the_actual_edge_lines_instead_of_its_paragraph_envelope() {
        let frame = NSRect::new(NSPoint::new(0.0, 500.0), NSSize::new(700.0, 20_000.0));
        let origin = NSPoint::new(36.0, 32.0);
        let clip = NSRect::new(NSPoint::new(0.0, 5_532.0), NSSize::new(772.0, 400.0));
        assert_eq!(
            visible_line_edge_offset(frame, clip, origin, false),
            Some(5_000.5)
        );
        assert_eq!(
            visible_line_edge_offset(frame, clip, origin, true),
            Some(5_399.5)
        );
        let first = visible_line_source_range(
            &(100..18_100),
            &(100..18_100),
            NSRange::new(4_000, 80),
            18_100,
        )
        .unwrap();
        let last = visible_line_source_range(
            &(100..18_100),
            &(100..18_100),
            NSRange::new(5_520, 80),
            18_100,
        )
        .unwrap();
        // 源范围来自 TextKit 的两条真实行；20,000 点高的 paragraph 不进入返回窗口。
        assert_eq!(first, 4_100..4_180);
        assert_eq!(last, 5_620..5_700);
        assert_eq!(
            visible_line_source_window(&first, &last, 18_100),
            Some(4_100..5_700)
        );
        assert!(visible_line_intersects_clip(
            frame,
            NSRect::new(NSPoint::new(0.0, 4_990.0), NSSize::new(600.0, 20.0)),
            clip,
            origin,
        )); // 顶部残行仍是真实可见行，不能整行略掉。
    }

    #[test]
    fn element_origin_and_original_fragment_bounds_reject_stale_or_misassociated_lines() {
        assert_eq!(
            visible_line_source_range(&(100..300), &(180..240), NSRange::new(90, 30), 400),
            Some(190..220)
        );
        for line in [
            NSRange::new(0, 20),
            NSRange::new(130, 20),
            NSRange::new(90, 0),
        ] {
            // 局部行不能加到 fragment 原点，空行和越过原 fragment 的范围也不产生源权限。
            assert!(visible_line_source_range(&(100..300), &(180..240), line, 400).is_none());
        }
        assert!(
            visible_line_source_range(&(100..300), &(180..240), NSRange::new(90, 30), 299)
                .is_none()
        );
        assert!(
            visible_line_source_range(
                &(usize::MAX - 4..usize::MAX),
                &(usize::MAX - 4..usize::MAX),
                NSRange::new(3, 2),
                usize::MAX
            )
            .is_none()
        );
        assert_eq!(
            visible_line_source_window(&(190..220), &(190..220), 300),
            Some(190..220)
        );
        for (first, last) in [
            (220..240, 190..210),
            (190..220, 200..230),
            (190..190, 220..240),
        ] {
            // 反向、非同一行的重叠或空范围都不能被 union 掩盖。
            assert!(visible_line_source_window(&first, &last, 300).is_none());
        }
        assert!(visible_line_source_window(&(190..220), &(240..301), 300).is_none());
    }

    #[test]
    fn short_document_blank_tail_uses_only_an_intersecting_existing_line() {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(700.0, 24.0));
        let clip = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(772.0, 700.0));
        let origin = NSPoint::new(36.0, 32.0);
        assert_eq!(
            visible_line_edge_offset(frame, clip, origin, true),
            Some(23.5)
        );
        assert!(visible_line_intersects_clip(frame, frame, clip, origin));
        let offscreen = NSRect::new(NSPoint::new(0.0, 5_000.0), frame.size);
        assert!(visible_line_edge_offset(offscreen, clip, origin, true).is_none());
        assert!(!visible_line_intersects_clip(
            offscreen, frame, clip, origin
        ));
        assert!(!visible_line_intersects_clip(
            frame,
            NSRect::new(NSPoint::new(0.0, f64::NAN), frame.size),
            clip,
            origin,
        ));
        assert!(
            visible_line_edge_offset(frame, clip, NSPoint::new(0.0, f64::INFINITY), false)
                .is_none()
        );
        assert!(visible_line_edge_offset(frame, NSRect::ZERO, origin, false).is_none());
    }
}

#[cfg(test)]
mod mode_viewport_tests {
    use super::*;

    fn source_stamp() -> ModeViewportStamp {
        ModeViewportStamp {
            owner: 7,
            generation: 12,
            length: 100,
            width: 800,
            before: MarkdownPresentationMode::Source,
            after: MarkdownPresentationMode::InlinePreview,
            input: 20,
            find_input: 30,
            header: 40,
            workspace_input: 50,
            selection: MarkdownTextSelection {
                anchor_utf16: 60,
                head_utf16: 50,
            },
        }
    }

    #[test]
    fn mode_geometry_keeps_visible_source_screen_y_and_horizontal_clip() {
        let frame = NSRect::new(NSPoint::new(12.0, 2_400.0), NSSize::new(14.0, 22.0));
        let clip = NSRect::new(NSPoint::new(0.0, 1_000.0), NSSize::new(700.0, 500.0));
        let target = mode_probe_scroll_origin(frame, clip, 180.0).unwrap();
        assert_eq!(target, NSPoint::new(0.0, 2_220.0));
        assert!(mode_probe_is_visible(frame, NSRect::new(target, clip.size)));
        assert!(!mode_probe_is_visible(frame, clip));
    }

    #[test]
    fn visible_line_sampling_and_source_origin_preserve_the_actual_fragment() {
        let clip = NSRect::new(NSPoint::new(-74.0, 5_882.0), NSSize::new(720.0, 520.0));
        let origin = NSPoint::new(16.0, 12.0);
        let short = NSRect::new(NSPoint::new(0.0, 5_870.0), NSSize::new(700.0, 20.0));
        assert_eq!(mode_visible_line_offset(short, clip, origin), Some(10.0));
        let giant = NSRect::new(NSPoint::new(0.0, 5_000.0), NSSize::new(700.0, 2_000.0));
        assert_eq!(mode_visible_line_offset(giant, clip, origin), Some(1_130.0));
        assert!(mode_visible_line_offset(short, clip, NSPoint::new(0.0, f64::INFINITY)).is_none());
        assert!(
            mode_visible_line_offset(
                short,
                NSRect::new(clip.origin, NSSize::new(0.0, 520.0)),
                origin
            )
            .is_none()
        );
        // line range相对element，不能加到可能从段落中间开始的fragment起点。
        assert_eq!(
            mode_line_source_candidate(&(100..200), &(150..180), NSRange::new(55, 10)),
            Some(155)
        );
        assert_eq!(
            mode_line_source_candidate(&(100..200), &(150..180), NSRange::new(0, 10)),
            None
        );
        assert_eq!(
            mode_line_source_candidate(&(100..200), &(150..180), NSRange::new(75, 10)),
            None
        );
        // 窗口末端的完整字符不能越界，真正EOF空caret仍可停在末端。
        assert!(mode_probe_belongs_to_window(&(9..10), &(0..10)));
        assert!(!mode_probe_belongs_to_window(&(10..11), &(0..10)));
        assert!(mode_probe_belongs_to_window(&(10..10), &(0..10)));
    }

    #[test]
    fn ruler_clip_exposure_does_not_change_the_document_column_width() {
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(646.0, 7_000.0));
        let ruler_clip = NSRect::new(NSPoint::new(-74.0, 5_882.0), NSSize::new(720.0, 520.0));
        assert!(mode_content_column_is_visible(
            bounds, ruler_clip, ruler_clip
        ));
        // 同样宽度却右移一列像素不能证明左端已暴露，不能仅比较交集的size。
        let shifted = NSRect::new(NSPoint::new(1.0, 5_882.0), NSSize::new(646.0, 520.0));
        assert!(!mode_content_column_is_visible(bounds, shifted, shifted));
        let narrow = NSRect::new(NSPoint::new(0.0, 5_882.0), NSSize::new(645.0, 520.0));
        assert!(!mode_content_column_is_visible(bounds, ruler_clip, narrow));
        // Preview无ruler仍要求同一完整正文列；非法clip不允许先裁剪再接受。
        let preview_bounds = NSRect::new(bounds.origin, NSSize::new(720.0, 7_000.0));
        let preview_clip = NSRect::new(NSPoint::new(0.0, 5_882.0), NSSize::new(720.0, 520.0));
        assert!(mode_content_column_is_visible(
            preview_bounds,
            preview_clip,
            preview_clip
        ));
        let infinite = NSRect::new(preview_clip.origin, NSSize::new(f64::INFINITY, 520.0));
        assert!(!mode_content_column_is_visible(
            preview_bounds,
            preview_clip,
            infinite
        ));
    }

    #[test]
    fn mode_geometry_handles_eof_caret_and_rejects_nonfinite_pixels() {
        let caret = NSRect::new(NSPoint::new(0.0, 10.0), NSSize::new(1.0, 20.0));
        let clip = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(800.0, 500.0));
        assert_eq!(
            mode_probe_scroll_origin(caret, clip, 300.0),
            Some(NSPoint::new(0.0, 0.0))
        );
        assert!(mode_probe_scroll_origin(caret, clip, f64::NAN).is_none());
        assert!(
            mode_probe_scroll_origin(
                caret,
                NSRect::new(clip.origin, NSSize::new(0.0, 500.0)),
                0.0
            )
            .is_none()
        );
        assert!(!mode_probe_is_visible(
            NSRect::new(NSPoint::new(0.0, f64::INFINITY), caret.size),
            clip
        ));
    }

    #[test]
    fn ruler_output_width_is_permitted_only_during_owned_transition() {
        let stamp = source_stamp();
        assert!(mode_viewport_phase_permits(
            stamp,
            ModeViewportPhase::Capturing,
            stamp.before,
            800
        ));
        assert!(!mode_viewport_phase_permits(
            stamp,
            ModeViewportPhase::Capturing,
            stamp.before,
            820
        ));
        assert!(mode_viewport_phase_permits(
            stamp,
            ModeViewportPhase::Changing,
            stamp.after,
            820
        ));
        assert!(mode_viewport_phase_permits(
            stamp,
            ModeViewportPhase::Ready(820),
            stamp.after,
            820
        ));
        assert!(!mode_viewport_phase_permits(
            stamp,
            ModeViewportPhase::Ready(820),
            stamp.after,
            840
        ));
        assert!(!mode_viewport_phase_permits(
            stamp,
            ModeViewportPhase::Ready(820),
            stamp.before,
            820
        ));
        let reverse = ModeViewportStamp {
            before: stamp.after,
            after: stamp.before,
            ..stamp
        };
        assert!(mode_viewport_phase_permits(
            reverse,
            ModeViewportPhase::Ready(760),
            reverse.after,
            760
        ));
    }

    #[test]
    fn owned_ruler_width_does_not_accept_new_input_window_or_owner() {
        let stamp = source_stamp();
        let actual = ModeViewportAuthority {
            owner: stamp.owner,
            mode: stamp.after,
            width: 820,
            input: stamp.input,
            find_input: stamp.find_input,
            header: stamp.header,
            workspace_input: stamp.workspace_input,
        };
        assert!(mode_viewport_authority_permits(
            stamp,
            ModeViewportPhase::Changing,
            actual
        ));
        for invalid in [
            ModeViewportAuthority { owner: 8, ..actual },
            ModeViewportAuthority {
                input: 21,
                ..actual
            },
            ModeViewportAuthority {
                find_input: 31,
                ..actual
            },
            ModeViewportAuthority {
                header: 41,
                ..actual
            },
            ModeViewportAuthority {
                workspace_input: 51,
                ..actual
            },
        ] {
            assert!(!mode_viewport_authority_permits(
                stamp,
                ModeViewportPhase::Changing,
                invalid
            ));
        }
    }

    #[test]
    fn real_single_cell_receipt_advances_only_exact_next_generation() {
        let stamp = source_stamp();
        let edit = MarkdownHistoryTextEdit {
            replaced_utf16_range: 20..30,
            change_in_utf16_length: -4,
            selection_after: MarkdownTextSelection::caret(26),
        };
        let next = mode_stamp_after_owned_cell(stamp, &edit, 13, 96).unwrap();
        assert_eq!(next.selection, edit.selection_after);
        assert_eq!(next.owner, stamp.owner);
        assert_eq!(next.input, stamp.input);
        assert_eq!(next.find_input, stamp.find_input);
        assert_eq!(next.header, stamp.header);
        assert_eq!(next.before, stamp.before);
        assert!(mode_stamp_after_owned_cell(stamp, &edit, 14, 96).is_none());
        assert!(mode_stamp_after_owned_cell(stamp, &edit, 13, 95).is_none());
        assert!(
            mode_stamp_after_owned_cell(
                ModeViewportStamp {
                    generation: u64::MAX,
                    ..stamp
                },
                &edit,
                0,
                96
            )
            .is_none()
        );
    }

    #[test]
    fn source_anchor_maps_real_cell_shrink_insert_and_deleted_target() {
        let edit = MarkdownHistoryTextEdit {
            replaced_utf16_range: 20..30,
            change_in_utf16_length: -4,
            selection_after: MarkdownTextSelection::caret(26),
        };
        assert_eq!(mode_map_cell_position(10, &edit, false), Some(10));
        assert_eq!(mode_map_cell_position(25, &edit, false), Some(20));
        assert_eq!(mode_map_cell_position(25, &edit, true), Some(26));
        assert_eq!(mode_map_cell_position(30, &edit, false), Some(26));
        assert_eq!(mode_map_cell_position(100, &edit, true), Some(96));
        let insert = MarkdownHistoryTextEdit {
            replaced_utf16_range: 20..20,
            change_in_utf16_length: 3,
            selection_after: MarkdownTextSelection::caret(23),
        };
        assert_eq!(mode_map_cell_position(20, &insert, false), Some(20));
        assert_eq!(mode_map_cell_position(20, &insert, true), Some(23));
    }

    #[test]
    fn mode_target_does_not_expand_far_source_into_unbounded_ensure() {
        assert!(
            native_markdown_bounded_table_layout_range(&(1_000..1_500), &(1_200..1_201)).is_some()
        );
        assert!(
            native_markdown_bounded_table_layout_range(&(1_000..1_500), &(140_000..140_001))
                .is_none()
        );
    }
}
