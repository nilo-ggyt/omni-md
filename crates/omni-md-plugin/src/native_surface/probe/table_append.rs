//! ACTIVE小表的程序动作回归；原请求终点驱动事件，不用slot消失或布局轮询推测成功。

use super::super::table_editor::NativeMarkdownTableCellPointer;
use super::*;
use objc2::Message;
use omni_markdown_core::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource;

const OVER_BUDGET_UNITS: usize = 65_537;
const CONFLICT_DRAFT: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 甲乙 |\n";
const CONFLICT_EXTERNAL: &str = "# 外部版本甲\n\n磁盘内容由独立写入者修改，原本地草稿必须保留。\n";
const CONFLICT_RELOADED: &str = "# 外部版本乙\n\n确认重载后应显示这一实际磁盘版本。\n";
const CONFLICT_COPY_TITLE: &str = "冲突草稿副本";
const CONFLICT_COPY_PATH: &str = "冲突草稿副本.md";
const POINTER_CELL: &str = "**Alpha 中😀后** `code` \\| ~~A אבג 😀 Z~~";
const POINTER_DISPLAY: &str = "Alpha 中😀后 code | A אבג 😀 Z";
const FIND_REPLACEMENT_BEFORE: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 原值 |\n";
const FIND_REPLACEMENT_AFTER: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 原中文 |\n";

thread_local! {
    /// 只有当前合成操作的弱票；生产未武装时，两个实际就绪通知没有任何动作。
    static CONFLICT_OPERATION: RefCell<std::rc::Weak<ConflictProbeOperation>> =
        const { RefCell::new(std::rc::Weak::new()) };
    /// 唯一原合成 Down 的弱票；普通产品未武装时没有资源或回调。
    static POINTER_OPERATION: RefCell<std::rc::Weak<PointerProbeOperation>> =
        const { RefCell::new(std::rc::Weak::new()) };
}

/// 预声明的单次真实鼠标动作；只持 Weak 与有限坐标，不拥有窗口、正文或字段编辑器。
struct PointerProbeOperation {
    document: WeakMarkdownDocument,
    view: Weak<PieceBackedMarkdownTextView>,
    view_identity: usize,
    coordinate: NativeMarkdownTableCellCoordinate,
    source: std::ops::Range<usize>,
    down_identity: usize,
    window_point: NSPoint,
    window_number: isize,
    timestamp: f64,
    count: isize,
    shift: bool,
    body: bool,
    pointer: RefCell<Option<std::rc::Weak<NativeMarkdownTableCellPointer>>>,
    delivered: Cell<bool>,
    terminal: Rc<dyn Fn(i16)>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ConflictProbeAction {
    CancelReload,
    CopyDraft,
    ReturnOriginal,
    ConfirmReload,
}

/// 一次明确实际操作的收据；不保活controller、原Doc、NSAlert、按钮或字段。
struct ConflictProbeOperation {
    controller: Weak<NativeMarkdownController>,
    original: NativeMarkdownCurrentNoteIntent,
    action: ConflictProbeAction,
    destination: &'static str,
    terminal: Rc<dyn Fn(i16)>,
    next_stage: i16,
    active: Cell<bool>,
    modal_seen: Cell<bool>,
    delivered: Cell<bool>,
    modal_result: RefCell<Option<Result<(), String>>>,
}

impl ConflictProbeOperation {
    /// 实际按钮返回后主队列才交付取消；复制/重载必须另外等真实新Doc安装。
    fn deliver(&self) {
        if self.active.get() && !self.delivered.replace(true) {
            (self.terminal)(self.next_stage);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConflictDocumentSnapshot {
    revision: omni_markdown_core::MarkdownDocumentRevision,
    generation: u64,
    selection: MarkdownTextSelection,
    dirty: bool,
    undo: bool,
    redo: bool,
}

impl ConflictDocumentSnapshot {
    /// 仅取原根和历史标量；不请求全文、布局或另一份正文。
    fn capture(document: &MarkdownDocumentBuffer) -> Result<Self, String> {
        Ok(Self {
            revision: document
                .current_revision()
                .map_err(|cause| cause.to_string())?,
            generation: document.generation(),
            selection: document.selection(),
            dirty: document.is_dirty(),
            undo: document.can_undo(),
            redo: document.can_redo(),
        })
    }
}

struct ConflictProbeFacts {
    original: NativeMarkdownCurrentNoteIntent,
    publication: u64,
    snapshot: ConflictDocumentSnapshot,
    current_document: WeakMarkdownDocument,
    current_path: &'static str,
    current_text: &'static str,
    original_disk_text: &'static str,
    bar: Weak<NSBox>,
    completed: bool,
}

/// 查找组合回归只跨事件保弱身份和预声明坐标；不保活栏、字段或共享 editor。
struct FindReplacementProbeFacts {
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    field_editor: Weak<NSTextView>,
    route: Option<(usize, u64)>,
    intent: u64,
    first: MarkdownTextSelection,
    last: MarkdownTextSelection,
}

/// 仅独立原生回归入口可选；产品导航与预算不读取此测试枚举。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeMarkdownTableAppendCase {
    /// 真实提交一次，再以正文程序Undo/Redo恢复基线。
    #[default]
    Positive,
    /// 超过Native字段的UTF16准入一unit，必须保留整份原草稿而不提交。
    Budget,
    /// 原请求仍Pending时同步取消，不能由迟到executor提交或重新打开格子。
    Cancel,
    /// 原格已有加粗，真实草稿删标记再加粗必须无字符事务地结束字段。
    FormatUnchanged,
    /// 原追加后的新空格已实际辞任；普通提交仍须正常清格且保留原字符历史。
    CommitResigned,
    /// 格内只选“中😀”加粗，验证同次源替换、真实after、Undo/Redo与生产保存。
    FormatSelection,
    /// 相同格内选区由实际粗体、撤销和重做按钮驱动，另核真实窗口缩放不改变正文。
    ToolbarFormatSelection,
    /// 两个真实字段输入组先走本地Undo，再以工具栏Redo/Undo恢复草稿，最后只提交一次canonical。
    ToolbarFieldHistory,
    /// 原 preview glyph 的真实 Down/Ready/Up、选择粒度、Shift、drag 与实际删除线像素。
    PointerSelection,
    /// 替换框实际组合输入不阻断导航；真实提交、替换及正文历史仍保持准确。
    FindReplacementComposition,
}

fn cell_activation_terminal(_outcome: NativeMarkdownTableCellOutcome) {
    // 唯一终点只投507；原Rc的结果由后继事件认证，不在回调内读取Native字段。
    crate::runtime_probe_application::post_table_cell_activation_terminal();
}

fn append_terminal(_outcome: NativeMarkdownTableAppendOutcome) {
    // 终点只投递driver事件，不在这里领取权限或运行断言。
    crate::runtime_probe_application::post_table_append_terminal();
}

#[derive(Clone)]
struct AppendFacts {
    owner: usize,
    document: WeakMarkdownDocument,
    generation: u64,
    width: i64,
    baseline: omni_markdown_core::MarkdownDocumentRevision,
    selection: MarkdownTextSelection,
    after: Option<omni_markdown_core::MarkdownDocumentRevision>,
    after_selection: Option<MarkdownTextSelection>,
}

/// 自有合成copy可被正常保存；不沿用长文只读工作负载的inode/mtime不变断言。
pub struct NativeMarkdownTableAppendWorkload {
    case: NativeMarkdownTableAppendCase,
    workspace: omni_markdown_core::MarkdownWorkspace,
    path: std::path::PathBuf,
    lifecycle: Rc<MemoryWorkloadLifecycle>,
    facts: RefCell<Option<AppendFacts>>,
    receipt: RefCell<Option<Rc<NativeMarkdownTableAppendReceipt>>>,
    activation_receipt: RefCell<Option<Rc<NativeMarkdownTableCellReceipt>>>,
    field_history_editor: RefCell<Option<Rc<Weak<NSTextView>>>>,
    /// 只观察这张实际信息卡的首次身份；重复开关须复用，最终关闭必须真实nil。
    information_root: RefCell<Option<Weak<NSBox>>>,
    conflict: RefCell<Option<ConflictProbeFacts>>,
    conflict_operation: RefCell<Option<Rc<ConflictProbeOperation>>>,
    terminal: Rc<dyn Fn(i16)>,
    pointer_round: Cell<u8>,
    pointer_operation: RefCell<Option<Rc<PointerProbeOperation>>>,
    stale_pointer: RefCell<Option<std::rc::Weak<NativeMarkdownTableCellPointer>>>,
    find_replacement: RefCell<Option<FindReplacementProbeFacts>>,
}

fn step<T>(current: &impl Fn() -> bool, work: impl FnOnce() -> T) -> Result<T, String> {
    native_markdown_table_intent_step(current, work)
        .map_err(|_| "追加回归的原窗口或正文权限已改变".to_owned())
}

impl NativeMarkdownTableAppendWorkload {
    /// 原 Down 已通过产品 hit/source 门禁后绑定同一 Pointer；预武装整数本身不能签发 Ready。
    pub(in crate::native_surface) fn observe_pointer_begin(
        view: &PieceBackedMarkdownTextView,
        pointer: &Rc<NativeMarkdownTableCellPointer>,
        document: &WeakMarkdownDocument,
        coordinate: NativeMarkdownTableCellCoordinate,
        source: &std::ops::Range<usize>,
    ) {
        let operation = POINTER_OPERATION.with_borrow(std::rc::Weak::upgrade);
        let Some(operation) = operation else {
            return;
        };
        if operation.view_identity == std::ptr::from_ref(view).addr()
            && WeakMarkdownDocument::ptr_eq(&operation.document, document)
            && operation.coordinate == coordinate
            && operation.source == *source
            && pointer.probe_down_identity() == Some(operation.down_identity)
            && operation.pointer.borrow().is_none()
        {
            operation.pointer.replace(Some(Rc::downgrade(pointer)));
        } // 不匹配的真实事件保持产品原行为，不能被验收入口绑定到别的票。
    }

    /// 只由产品真实字段激活/正文扩选安装返回调用；弱票认原 Down 对应的同一 Pointer allocation。
    pub(in crate::native_surface) fn observe_pointer_ready(
        view: &PieceBackedMarkdownTextView,
        pointer: &NativeMarkdownTableCellPointer,
        body: bool,
    ) {
        let operation = POINTER_OPERATION.with_borrow(std::rc::Weak::upgrade);
        let Some(operation) = operation else {
            return;
        };
        let same = operation
            .pointer
            .borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
            .is_some_and(|original| std::ptr::eq(&*original, pointer));
        if same
            && operation.view_identity == std::ptr::from_ref(view).addr()
            && operation.body == body
            && !operation.delivered.replace(true)
        {
            (operation.terminal)(507); // 沿原 driver 事件交付；回调不创建字段或替代实际 Ready。
        }
    }

    pub(crate) fn pointer_is_finished(&self) -> bool {
        self.pointer_round.get() == 6
    }

    /// 只借当前实际可见矩阵；原 Rc 在 ivar 借用外 clone cell，避免 Native retain 重入 RefCell。
    fn pointer_cell(
        view: &PieceBackedMarkdownTextView,
        column: usize,
        current: &impl Fn() -> bool,
    ) -> Result<NativeMarkdownVisibleTableCellLayout, String> {
        let key = step(current, || view.current_table_layout_request_key())?
            .ok_or("鼠标回归缺少布局 key")?;
        let cache = view
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.request_key == key)
            .map(Rc::clone)
            .ok_or("鼠标回归尚无原可见矩阵")?;
        let cell = cache
            .tables
            .iter()
            .flat_map(|table| &table.cells)
            .find(|cell| cell.coordinate.row_index == 1 && cell.coordinate.column_index == column)
            .cloned()
            .ok_or("鼠标回归缺少原目标格")?;
        if !current() {
            return Err("原鼠标几何已撤权".into());
        }
        Ok(cell)
    }

    /// 每个 case 从真实取消边界归还原字段，再由原系统动作把正文 anchor 回到 0。
    fn prepare_pointer_round(&self) -> Result<(), String> {
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let editor = surface.editor.as_ref().ok_or("鼠标回归没有编辑器")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            step(&current, || view.cancel_active_table_cell())?;
            // SAFETY: 既有 Cocoa 文档导航动作，不新增按键；原 view 在本主线程同步调用期存活。
            step(&current, || unsafe {
                let _: () = msg_send![&**view, moveToBeginningOfDocument: None::<&AnyObject>];
            })?;
            let document = editor.document().borrow();
            if document.selection() != MarkdownTextSelection::caret(0)
                || document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != facts.baseline
                || document.can_undo()
                || document.can_redo()
            {
                return Err("鼠标回归的原 anchor、正文或历史没有回到预声明基线".into());
            }
            Ok(())
        })
    }

    /// 从原 CTLine 实际 caret 生成真实 NSEvent；所有 Surface 借用结束后调用者才 sendEvent。
    fn arm_pointer_click(
        &self,
        column: usize,
        count: isize,
        shift: bool,
        time: f64,
    ) -> Result<(Retained<NSEvent>, Retained<NSEvent>), String> {
        let facts = self.facts()?;
        let (down, up, operation) = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let editor = surface.editor.as_ref().ok_or("鼠标回归没有编辑器")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            let cell = Self::pointer_cell(view, column, &current)?;
            let display_index = if column == 0 {
                0
            } else {
                match self.pointer_round.get() {
                    1 => 2, // “Alpha”中部，由真实 Native word granularity 决定完整词。
                    2 => POINTER_DISPLAY[..POINTER_DISPLAY.find("code").unwrap()]
                        .encode_utf16()
                        .count(),
                    3 => 0, // 隐藏粗体开标记后的第一个显示 caret。
                    _ => POINTER_DISPLAY[..POINTER_DISPLAY.find('中').unwrap()]
                        .encode_utf16()
                        .count(),
                }
            };
            let point = step(&current, || {
                cell.text_layout
                    .probe_point_for_display_index(display_index, cell.text_frame)
            })?
            .ok_or("原 CTLine 没有目标 glyph caret")?;
            let point = step(&current, || view.convertPoint_toView(point, None))?;
            let number = step(&current, || surface.panel.windowNumber())?;
            let flags = if shift {
                NSEventModifierFlags::Shift
            } else {
                NSEventModifierFlags::empty()
            };
            let down = step(&current, || NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                NSEventType::LeftMouseDown, point, flags, time, number, None, (time * 1000.0) as isize, count, 1.0,
            ))?.ok_or("AppKit 未创建原鼠标 Down")?;
            let up = step(&current, || NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                NSEventType::LeftMouseUp, point, flags, time + 0.001, number, None, (time * 1000.0) as isize + 1, count, 0.0,
            ))?.ok_or("AppKit 未创建原鼠标 Up")?;
            let operation = Rc::new(PointerProbeOperation {
                document: facts.document.clone(),
                view: Weak::from(&**view),
                view_identity: std::ptr::from_ref(&**view).addr(),
                coordinate: cell.coordinate,
                source: cell.source_utf16_range,
                down_identity: std::ptr::from_ref(&*down).addr(),
                window_point: point,
                window_number: number,
                timestamp: time,
                count,
                shift,
                body: shift,
                pointer: RefCell::new(None),
                delivered: Cell::new(false),
                terminal: Rc::clone(&self.terminal),
            });
            Ok((down, up, operation))
        })?;
        POINTER_OPERATION.with_borrow_mut(|slot| *slot = Rc::downgrade(&operation));
        let old = self.pointer_operation.replace(Some(operation));
        drop(old);
        Ok((down, up))
    }

    /// 原 Down 与 Up 通过真实 NSApplication→NSWindow→view；先 Up 负载不运行或伪造 executor。
    pub(crate) fn begin_pointer_case(&self) -> Result<(), String> {
        self.prepare_pointer_round()?;
        let round = self.pointer_round.get();
        let mtm = MainThreadMarker::new().expect("鼠标回归位于 AppKit 主线程");
        let application = NSApplication::sharedApplication(mtm);
        if round == 5 {
            let (down, _up) = self.arm_pointer_click(0, 1, false, 105.0)?;
            application.sendEvent(&down);
            self.verify_pointer_pending()?;
            let original = self
                .pointer_operation
                .borrow()
                .as_ref()
                .unwrap()
                .pointer
                .borrow()
                .clone()
                .ok_or("原 Down 没有绑定 Pointer Weak")?;
            self.stale_pointer.replace(Some(original));
            // 第二个真实 Down 撤销前票；保留原 Weak 验证，不能手动清产品槽或伪造取消。
        }
        let count = match round {
            1 => 2,
            2 => 3,
            _ => 1,
        };
        let (down, up) = self.arm_pointer_click(1, count, round == 4, 110.0 + f64::from(round))?;
        application.sendEvent(&down);
        if matches!(round, 0 | 5) {
            self.verify_pointer_pending()?;
            application.sendEvent(&up); // 与 Down 同栈；异步实际 Ready 只能在二者之后交付。
        }
        Ok(())
    }

    fn verify_pointer_pending(&self) -> Result<(), String> {
        let operation = self
            .pointer_operation
            .borrow()
            .clone()
            .ok_or("原鼠标票缺失")?;
        with_active_native_markdown_surface(|surface| {
            let view = &surface
                .editor
                .as_ref()
                .ok_or("原编辑器缺失")?
                .backend
                .text_view;
            if operation.delivered.get()
                || operation.pointer.borrow().is_none()
                || view.ivars().active_table_cell.borrow().is_some()
                || view.ivars().pending_table_cell_activation.get() != Some(operation.coordinate)
            {
                return Err("先 Up 场景未真实处于原 Pointer Pending".into());
            }
            Ok(())
        })
    }

    /// 已完成一组真实事件后才进入下一固定业务场景，最后沿原 522/523 关闭。
    pub(crate) fn advance_pointer_case(&self) -> Result<(), String> {
        self.pointer_round.set(self.pointer_round.get() + 1);
        if !self.pointer_is_finished() {
            self.begin_pointer_case()?;
        }
        Ok(())
    }

    /// 后续真实事件保留原窗口/时间/次数/修饰键；不再创建 Down 或重新签票。
    fn pointer_event(
        operation: &PointerProbeOperation,
        kind: NSEventType,
        point: NSPoint,
        elapsed: f64,
    ) -> Result<Retained<NSEvent>, String> {
        let flags = if operation.shift {
            NSEventModifierFlags::Shift
        } else {
            NSEventModifierFlags::empty()
        };
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind, point, flags, operation.timestamp + elapsed, operation.window_number, None,
            (operation.timestamp * 1000.0) as isize + 2, operation.count, if kind == NSEventType::LeftMouseUp { 0.0 } else { 1.0 },
        ).ok_or_else(|| "AppKit 未创建原鼠标后续事件".into())
    }

    /// 固定期望来自独立合成 Markdown；不能把产品当前 selectedRange 当预期值。
    fn pointer_expected_range(&self) -> NSRange {
        let source_offset = |needle: &str| {
            POINTER_CELL[..POINTER_CELL.find(needle).unwrap()]
                .encode_utf16()
                .count()
        };
        match self.pointer_round.get() {
            1 => NSRange::new(2, 5), // 双击选 Alpha，不能包含隐藏的两个星号。
            2 => NSRange::new(0, POINTER_CELL.encode_utf16().count()), // 原生段落粒度覆盖真实 raw field。
            3 => NSRange::new(2, 0), // 首次 caret 恰在粗体内容前，随后才真实拖选。
            _ => NSRange::new(source_offset("中"), 0),
        }
    }

    fn verify_pointer_field(&self, expected: NSRange) -> Result<(), String> {
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let editor = surface.editor.as_ref().ok_or("鼠标回归编辑器已关闭")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            let field = step(&current, || view.existing_table_cell_editor())?
                .ok_or("真实 Ready 没有字段")?;
            let actual = step(&current, || field.currentEditor())?
                .and_then(|value| value.downcast::<NSTextView>().ok())
                .ok_or("真实 Ready 没有实际 field editor")?;
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
            let selected = step(&current, || actual.selectedRange())?;
            let value = step(&current, || actual.string())?;
            if selected != expected
                || !step(&current, || {
                    value.isEqualToString(&NSString::from_str(POINTER_CELL))
                })?
                || step(&current, || input.hasMarkedText())?
            {
                return Err(format!(
                    "原鼠标字段选择/内容/IME不符：round={} expected={expected:?} actual={selected:?}",
                    self.pointer_round.get()
                ));
            }
            Ok(())
        })
    }

    /// 原 Ready 到达后核真实 field/body；原数据和历史保持，再发送实际 drag/up。
    pub(crate) fn verify_pointer_ready(&self) -> Result<(), String> {
        let operation = self
            .pointer_operation
            .borrow()
            .clone()
            .ok_or("原鼠标票缺失")?;
        if !operation.delivered.get() || operation.pointer.borrow().is_none() {
            return Err("未收到原 Down 对应同 Pointer Weak 的真实 Ready".into());
        }
        let round = self.pointer_round.get();
        if round == 4 {
            let facts = self.facts()?;
            with_active_native_markdown_surface(|surface| {
                let editor = surface.editor.as_ref().ok_or("原扩选正文已关闭")?;
                let expected = MarkdownTextSelection {
                    anchor_utf16: 0,
                    head_utf16: operation.source.start + self.pointer_expected_range().location,
                };
                if !self.current(surface, &facts, facts.generation)
                    || editor.document().borrow().selection() != expected
                    || editor
                        .backend
                        .text_view
                        .ivars()
                        .active_table_cell
                        .borrow()
                        .is_some()
                {
                    return Err("跨格 Shift 未保留原真实 anchor/目标，或错误创建了字段".into());
                }
                Ok(())
            })?;
        } else {
            self.verify_pointer_field(self.pointer_expected_range())?;
        }
        let mtm = MainThreadMarker::new().expect("鼠标后继位于原主线程");
        let application = NSApplication::sharedApplication(mtm);
        if round == 3 {
            let facts = self.facts()?;
            let point = with_active_native_markdown_surface(|surface| {
                let current = || self.current(surface, &facts, facts.generation);
                let view = &surface
                    .editor
                    .as_ref()
                    .ok_or("拖选没有原编辑器")?
                    .backend
                    .text_view;
                let cell = Self::pointer_cell(view, 1, &current)?;
                let index = POINTER_DISPLAY[..POINTER_DISPLAY.find('后').unwrap()]
                    .encode_utf16()
                    .count();
                let point = step(&current, || {
                    cell.text_layout
                        .probe_point_for_display_index(index, cell.text_frame)
                })?
                .ok_or("原拖选目标没有 glyph")?;
                step(&current, || view.convertPoint_toView(point, None))
            })?;
            let drag = Self::pointer_event(&operation, NSEventType::LeftMouseDragged, point, 0.1)?;
            let up = Self::pointer_event(&operation, NSEventType::LeftMouseUp, point, 0.2)?;
            application.sendEvent(&drag);
            application.sendEvent(&up);
            let end = POINTER_CELL[..POINTER_CELL.find('后').unwrap()]
                .encode_utf16()
                .count();
            self.verify_pointer_field(NSRange::new(2, end - 2))?;
            self.verify_same_cell_shift(&operation)?;
        } else if !matches!(round, 0 | 5) {
            let up = Self::pointer_event(
                &operation,
                NSEventType::LeftMouseUp,
                operation.window_point,
                0.2,
            )?;
            application.sendEvent(&up);
        }
        if operation
            .pointer
            .borrow()
            .as_ref()
            .is_some_and(|pointer| pointer.upgrade().is_some())
        {
            return Err("真实 Up 后原 Pointer/事件载荷仍被产品持有".into());
        }
        if round == 5
            && self
                .stale_pointer
                .borrow()
                .as_ref()
                .is_none_or(|pointer| pointer.upgrade().is_some())
        {
            return Err("后继真实 Down 未撤销并释放前一个 Pending Pointer".into());
        }
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("鼠标回归正文已关闭")?;
            let document = editor.document().borrow();
            if !self.current(surface, &facts, facts.generation)
                || document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != facts.baseline
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
            {
                return Err("鼠标操作改变了原 Markdown 或字符历史".into());
            }
            Ok(())
        })?;
        if round == 5 {
            self.verify_pointer_strike_pixels()?;
        }
        println!(
            "native_table_pointer round={round} actual_down_ready_up=true exact_selection=true original_data_history=true pointer_released=true physical_mouse=false formal=false"
        );
        Ok(())
    }

    /// 同格 Shift 由真实 field editor 自己解释原鼠标事件；Up 先入系统队列供标准 tracking 消费。
    fn verify_same_cell_shift(&self, original: &PointerProbeOperation) -> Result<(), String> {
        let facts = self.facts()?;
        let at = POINTER_CELL[..POINTER_CELL.find('Z').unwrap()]
            .encode_utf16()
            .count();
        let (down, up) = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let view = &surface
                .editor
                .as_ref()
                .ok_or("同格 Shift 没有 view")?
                .backend
                .text_view;
            let field =
                step(&current, || view.existing_table_cell_editor())?.ok_or("同格字段缺失")?;
            let actual = step(&current, || field.currentEditor())?
                .and_then(|value| value.downcast::<NSTextView>().ok())
                .ok_or("同格 editor 缺失")?;
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
            let mut actual_range = NSRange::new(NSNotFound as usize, 0);
            // SAFETY: 固定原字段的 Z 是完整 scalar，actual_range 在同步调用内可写。
            let rect = step(&current, || unsafe {
                input.firstRectForCharacterRange_actualRange(NSRange::new(at, 1), &mut actual_range)
            })?;
            if actual_range.location > at || actual_range.location + actual_range.length < at + 1 {
                return Err("同格真实 caret 几何未覆盖 Z".into());
            }
            let rect = step(&current, || surface.panel.convertRectFromScreen(rect))?;
            let point = NSPoint::new(rect.origin.x + 0.1, rect.origin.y + rect.size.height / 2.0);
            let event = |kind, time, pressure| {
                NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind, point, NSEventModifierFlags::Shift, time, original.window_number, None, 9900, 1, pressure,
            ).ok_or_else(|| "AppKit 未创建同格 Shift 实际事件".to_owned())
            };
            Ok((
                event(NSEventType::LeftMouseDown, original.timestamp + 0.3, 1.0)?,
                event(NSEventType::LeftMouseUp, original.timestamp + 0.4, 0.0)?,
            ))
        })?;
        let application = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        let before = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let view = &surface
                .editor
                .as_ref()
                .ok_or("Shift诊断缺少原编辑器")?
                .backend
                .text_view;
            let field = step(&current, || view.existing_table_cell_editor())?
                .ok_or("Shift诊断缺少原字段")?;
            let actual = step(&current, || field.currentEditor())?
                .and_then(|value| value.downcast::<NSTextView>().ok())
                .ok_or("Shift诊断缺少原field editor")?;
            step(&current, || actual.selectedRange())
        })?;
        let current_event = application
            .currentEvent()
            .map(|event| (event.r#type(), event.modifierFlags()));
        println!(
            "native_table_pointer_shift phase=before range={before:?} event_type={:?} event_modifiers={:?} current_event={current_event:?} formal=false",
            down.r#type(),
            down.modifierFlags()
        );
        application.postEvent_atStart(&up, true);
        application.sendEvent(&down);
        let after = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let view = &surface
                .editor
                .as_ref()
                .ok_or("Shift诊断缺少原编辑器")?
                .backend
                .text_view;
            let field = step(&current, || view.existing_table_cell_editor())?
                .ok_or("Shift诊断缺少原字段")?;
            let actual = step(&current, || field.currentEditor())?
                .and_then(|value| value.downcast::<NSTextView>().ok())
                .ok_or("Shift诊断缺少原field editor")?;
            step(&current, || actual.selectedRange())
        })?;
        let current_event = application
            .currentEvent()
            .map(|event| (event.r#type(), event.modifierFlags()));
        println!(
            "native_table_pointer_shift phase=after range={after:?} event_type={:?} event_modifiers={:?} current_event={current_event:?} formal=false",
            down.r#type(),
            down.modifierFlags()
        );
        self.verify_pointer_field(NSRange::new(2, at - 2))
    }

    /// 只抓实际可见格内空格上的小条；字形缓存存在不能代替真实删除线像素。
    fn verify_pointer_strike_pixels(&self) -> Result<(), String> {
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let view = &surface
                .editor
                .as_ref()
                .ok_or("删除线像素缺少原 view")?
                .backend
                .text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            step(&current, || view.cancel_active_table_cell())?;
            let cell = Self::pointer_cell(view, 1, &current)?;
            let text = step(&current, || cell.attributed_text.string())?;
            if !step(&current, || {
                text.isEqualToString(&NSString::from_str(POINTER_DISPLAY))
            })? {
                return Err("实际预览的隐藏标记/escape/RTL文字不符".into());
            }
            let index = POINTER_DISPLAY[..POINTER_DISPLAY.find("A אב").unwrap()]
                .encode_utf16()
                .count()
                + 1;
            let point = step(&current, || {
                cell.text_layout
                    .probe_strike_space_point(index, cell.text_frame)
            })?
            .ok_or("实际 RTL run 没有原删除线空间")?;
            let rect = NSRect::new(
                NSPoint::new(point.x - 1.0, point.y - 5.0),
                NSSize::new(2.0, 10.0),
            );
            if !NSPointInRect(point, cell.text_frame)
                || !NSPointInRect(point, step(&current, || view.visibleRect())?)
            {
                return Err("删除线像素点不在实际可见原格".into());
            }
            let bitmap = step(&current, || {
                view.bitmapImageRepForCachingDisplayInRect(rect)
            })?
            .ok_or("AppKit 未创建有限格 bitmap")?;
            step(&current, || {
                view.cacheDisplayInRect_toBitmapImageRep(rect, &bitmap)
            })?;
            let (width, height) = (
                step(&current, || bitmap.pixelsWide())?,
                step(&current, || bitmap.pixelsHigh())?,
            );
            if width <= 0 || height <= 0 || width > 16 || height > 80 {
                return Err("有限像素条尺寸不符".into());
            }
            let mut center_ink = false;
            let mut clear_margin = false;
            for y in 0..height {
                let color = step(&current, || bitmap.colorAtX_y(width / 2, y))?
                    .ok_or("实际 bitmap 像素不可读")?;
                let intensity = step(&current, || {
                    color
                        .redComponent()
                        .max(color.greenComponent())
                        .max(color.blueComponent())
                })?;
                if (y - height / 2).abs() <= 2 && intensity > 0.35 {
                    center_ink = true;
                }
                if y <= 1 && intensity < 0.25 {
                    clear_margin = true;
                }
            }
            if !center_ink || !clear_margin {
                return Err("真实 RTL 空格条没有删除线像素或空白对照".into());
            }
            println!(
                "native_table_pointer_rtl_strike actual_visible_cell_bitmap=true center_ink=true blank_control=true bounded_pixels={} physical_mouse=false formal=false",
                width * height
            );
            Ok(())
        })
    }

    pub(crate) fn start(
        root: &Path,
        case: NativeMarkdownTableAppendCase,
        finished: impl FnOnce() + 'static,
        terminal: impl Fn(i16) + 'static,
    ) -> Result<Self, String> {
        let workspace =
            omni_markdown_core::MarkdownWorkspace::new(root.join("vault"), root.join("recovery"));
        let session = workspace
            .open_session()
            .map_err(|_| "无法打开自有小表工作负载")?;
        let lifecycle = Rc::new(MemoryWorkloadLifecycle {
            accepted: Cell::new(None),
            completed: Cell::new(None),
            valid: Cell::new(true),
            finished: RefCell::new(Some(Box::new(finished))),
        });
        let mut host = MemoryWorkloadHost {
            root: root.to_owned(),
            lifecycle: Rc::clone(&lifecycle),
        };
        show_native_markdown_window(session, lifecycle.clone(), &mut host)?;
        Ok(Self {
            case,
            workspace,
            path: root.join("vault/工作负载.md"),
            lifecycle,
            facts: RefCell::new(None),
            receipt: RefCell::new(None),
            activation_receipt: RefCell::new(None),
            field_history_editor: RefCell::new(None),
            information_root: RefCell::new(None),
            conflict: RefCell::new(None),
            conflict_operation: RefCell::new(None),
            terminal: Rc::new(terminal),
            pointer_round: Cell::new(0),
            pointer_operation: RefCell::new(None),
            stale_pointer: RefCell::new(None),
            find_replacement: RefCell::new(None),
        })
    }

    fn facts(&self) -> Result<AppendFacts, String> {
        self.facts
            .borrow()
            .clone()
            .ok_or_else(|| "追加回归缺少原基线".to_owned())
    }

    /// 独立小表场景预先固定两处真实源坐标，再沿原字段协议开始组合和真实按钮导航。
    pub(crate) fn begin_find_replacement_composition(&self) -> Result<(), String> {
        let facts = self.facts()?;
        if self.case != NativeMarkdownTableAppendCase::FindReplacementComposition
            || self.find_replacement.borrow().is_some()
            || facts.selection != MarkdownTextSelection::caret(0)
        {
            return Err("查找组合场景缺少唯一文首基线".into());
        }
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, facts.generation) {
                return Err("查找组合初始原票已失效".into());
            }
            let document = surface
                .editor
                .as_ref()
                .ok_or("原正文缺失")?
                .document()
                .borrow();
            if document.byte_len() != FIND_REPLACEMENT_BEFORE.len()
                || document.utf16_len() != FIND_REPLACEMENT_BEFORE.encode_utf16().count()
                || document.selection() != facts.selection
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
            {
                return Err("查找组合的固定源或初始历史不符".into());
            }
            for (index, expected) in FIND_REPLACEMENT_BEFORE.encode_utf16().enumerate() {
                if document
                    .utf16_code_unit_at(index)
                    .map_err(|cause| cause.to_string())?
                    != expected
                {
                    return Err("查找组合初始字节不是原固定夹具".into());
                }
            } // 首个查找 Native 动作前核完原范围，后继不能用实际新选择替代它。
            Ok(())
        })?;
        // 坐标只从本用例固定字面量计算，绝不读实际全文或在动作结束后重采目标。
        let position = |byte: usize| {
            let start = FIND_REPLACEMENT_BEFORE[..byte].encode_utf16().count();
            MarkdownTextSelection {
                anchor_utf16: start,
                head_utf16: start + 1,
            }
        };
        let first = position(FIND_REPLACEMENT_BEFORE.find('值').unwrap());
        let last = position(FIND_REPLACEMENT_BEFORE.rfind('值').unwrap());
        show_native_markdown_find_bar(true)?; // 正式显示完成且 owner 已归还，才领取实际字段。
        let mut probe = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("查找组合缺少原正文")?;
            let bar = editor.find_bar.borrow().clone().ok_or("实际查找栏未显示")?;
            let intent = Cell::new(surface.controller.find_input_intent());
            let route = Cell::new(
                surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    .ok_or("查询缺少原会话路由")?,
            );
            let current = || {
                self.current(surface, &facts, facts.generation)
                    && surface.controller.find_input_intent() == intent.get()
                    && surface
                        .controller
                        .ivars()
                        .field_editor_provider
                        .current_input_client_identity()
                        == Some(route.get())
            };
            let fields = step(&current, || bar.fields())?;
            let query = &fields[0];
            let replacement = &fields[1];
            for field in &fields {
                let value = step(&current, || field.stringValue())?;
                if !step(&current, || value.isEqualToString(ns_string!("")))? {
                    return Err("新查找栏不是本用例的空输入基线".into());
                }
                drop(value); // 后继字段读取仍先核原票，临时原生值不跨输入动作。
            }
            let query_editor = step(&current, || query.currentEditor())?
                .ok_or("查询未获得真实字段编辑器")?
                .downcast::<NSTextView>()
                .map_err(|_| "查询字段编辑器类型错误")?;
            let value = step(&current, || NSString::from_str("值"))?;
            if std::ptr::from_ref(&*query_editor).addr() != route.get().0 {
                return Err("查询不是原会话当前输入客户".into());
            }
            let next = intent.get().checked_add(1).ok_or("查询输入意图溢出")?;
            // SAFETY: 原实际字段和固定一个 UTF16 单元存活，只替换已证明为空的查询。
            unsafe {
                ProtocolObject::<dyn NSTextInputClient>::from_ref(&*query_editor)
                    .insertText_replacementRange(&value, NSRange::new(0, 0));
            }
            if !self.current(surface, &facts, facts.generation)
                || surface.controller.find_input_intent() != next
                || surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    != Some(route.get())
            {
                return Err("真实查询输入未只推进原查询意图一次".into());
            }
            intent.set(next);
            drop((value, query_editor));
            if !current() {
                return Err("查询临时值归还后原输入已变化".into());
            }
            // SAFETY: 原替换字段仍属于同一栏；正常选择让 AppKit 交接输入、校验和本地历史。
            unsafe { replacement.selectText(None) };
            if !self.current(surface, &facts, facts.generation)
                || surface.controller.find_input_intent() != intent.get()
            {
                return Err("仅切换替换焦点改变了查询意图或原正文".into());
            }
            // selectText 是明确请求的真实客户切换；仅这次成功动作之后固定其后继路由。
            route.set(
                surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    .ok_or("替换缺少原会话路由")?,
            );
            let actual = step(&current, || replacement.currentEditor())?
                .ok_or("替换未获得真实字段编辑器")?
                .downcast::<NSTextView>()
                .map_err(|_| "替换字段编辑器类型错误")?;
            let session = actual
                .downcast_ref::<NativeMarkdownSessionFieldEditor>()
                .filter(|session| {
                    session.belongs_to(
                        &surface.controller.ivars().undo,
                        &surface.controller.ivars().owned,
                    )
                })
                .ok_or("替换输入不属于原会话")?;
            let actual_route = (
                std::ptr::from_ref(&*actual).addr(),
                session.input_client_revision(),
            );
            if actual_route != route.get() {
                return Err("替换字段不是预声明的实际交接客户".into());
            }
            let value = step(&current, || NSString::from_str("中"))?;
            let next = intent.get().checked_add(1).ok_or("替换组合意图溢出")?;
            // SAFETY: 实际替换 editor 接收固定候选；不发假通知，也不提前确认中文输入。
            unsafe {
                ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual)
                    .setMarkedText_selectedRange_replacementRange(
                        &value,
                        NSRange::new(1, 0),
                        NSRange::new(0, 0),
                    );
            }
            if !self.current(surface, &facts, facts.generation)
                || surface.controller.find_input_intent() != next
                || surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    != Some(route.get())
            {
                return Err("真实替换组合没有保持原客户或准确后态".into());
            }
            intent.set(next);
            let weak = Weak::from_retained(&actual);
            drop((value, actual, fields));
            if !current()
                || surface.controller.find_input_intent() != next
                || surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    != Some(route.get())
            {
                return Err("归还组合输入临时值后原客户已变化".into());
            }
            Ok(FindReplacementProbeFacts {
                bar: Rc::downgrade(&bar),
                field_editor: weak,
                route: Some(route.get()),
                intent: next,
                first,
                last,
            })
        })?;
        self.click_find_replacement_button(&facts, &mut probe, MarkdownFindAction::Next)?;
        self.find_replacement.replace(Some(probe));
        Ok(())
    }

    /// 原栏、Doc、源代次、输入意图及活客户共同撤权；后继事件不能重新签原操作票。
    fn find_replacement_current(
        &self,
        surface: &NativeMarkdownWindowSurface,
        facts: &AppendFacts,
        probe: &FindReplacementProbeFacts,
        generation: u64,
    ) -> bool {
        self.current(surface, facts, generation)
            && surface.controller.find_input_intent() == probe.intent
            && probe.route.is_none_or(|route| {
                surface
                    .controller
                    .ivars()
                    .field_editor_provider
                    .current_input_client_identity()
                    == Some(route)
            })
            && surface.editor.as_ref().is_some_and(|editor| {
                editor
                    .find_bar
                    .borrow()
                    .as_ref()
                    .is_some_and(|bar| std::rc::Weak::ptr_eq(&probe.bar, &Rc::downgrade(bar)))
            })
    }

    /// 原实际按钮在 Active 借用结束后点击；只预声明这次显式动作应推进的一次意图。
    fn click_find_replacement_button(
        &self,
        facts: &AppendFacts,
        probe: &mut FindReplacementProbeFacts,
        action: MarkdownFindAction,
    ) -> Result<(), String> {
        let button = with_active_native_markdown_surface(|surface| {
            let current = || self.find_replacement_current(surface, facts, probe, facts.generation);
            let bar = probe.bar.upgrade().ok_or("原查找栏已释放")?;
            step(&current, || bar.probe_button(action).retain())
        })?;
        let next = probe.intent.checked_add(1).ok_or("查找按钮意图溢出")?;
        // SAFETY: 只点击原栏当前具体按钮；原 target/action 自己排队执行，不伪造完成或直接调用后端。
        unsafe { button.performClick(None) };
        drop(button);
        probe.intent = next;
        Ok(())
    }

    /// 每个后继事件核对预声明源和真实 Native 选择，组合候选只核范围及状态，不读取候选文字。
    fn verify_find_replacement_stage(
        &self,
        facts: &AppendFacts,
        probe: &FindReplacementProbeFacts,
        stage: u8,
    ) -> Result<(), String> {
        let (delta, source, selection, dirty, undo, redo) = match stage {
            1 => (0, FIND_REPLACEMENT_BEFORE, probe.first, false, false, false),
            2 | 3 => (0, FIND_REPLACEMENT_BEFORE, probe.last, false, false, false),
            4 => (1, FIND_REPLACEMENT_AFTER, probe.first, true, true, false),
            5 => (2, FIND_REPLACEMENT_BEFORE, probe.last, false, false, true),
            6 => (3, FIND_REPLACEMENT_AFTER, probe.first, true, true, false),
            7 => (3, FIND_REPLACEMENT_AFTER, probe.first, false, true, false),
            _ => return Err("未知查找组合认证阶段".into()),
        }; // 三次字符代次仅来自 Replace、Undo、Redo；前后导航和拒绝不允许字符变化。
        let generation = facts
            .generation
            .checked_add(delta)
            .ok_or("查找组合代次溢出")?;
        let revision = with_active_native_markdown_surface(|surface| {
            let current = || self.find_replacement_current(surface, facts, probe, generation);
            if !current() {
                return Err("查找组合原操作票已失效".into());
            }
            let editor = surface.editor.as_ref().ok_or("查找组合原正文缺失")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if step(&current, || view.native_ordered_selection_range())?
                .map_err(|cause| cause.to_string())?
                != NSRange::new(
                    selection.anchor_utf16,
                    selection.head_utf16 - selection.anchor_utf16,
                )
            {
                return Err("查找组合没有选择预声明的准确 Native 匹配".into());
            }
            let bar = probe.bar.upgrade().ok_or("原查找栏已释放")?;
            let fields = step(&current, || bar.fields())?;
            let actual = step(&current, || fields[1].currentEditor())?;
            if stage <= 4 {
                let actual = actual
                    .ok_or("导航或替换辞任了原替换字段")?
                    .downcast::<NSTextView>()
                    .map_err(|_| "实际替换 editor 类型错误")?;
                let original =
                    step(&current, || probe.field_editor.load())?.ok_or("原替换 editor 已释放")?;
                let marked = stage <= 3;
                if !std::ptr::eq(&*actual, &*original)
                    || step(&current, || actual.hasMarkedText())? != marked
                    || step(&current, || actual.selectedRange())?
                        != NSRange::new(if marked { 1 } else { 2 }, 0)
                    || (marked && step(&current, || actual.markedRange())? != NSRange::new(0, 1))
                {
                    return Err("导航/拒绝改变了实际替换候选、局部选择或原客户".into());
                }
                if !marked {
                    let value = step(&current, || actual.string())?;
                    if !step(&current, || value.isEqualToString(ns_string!("中文")))? {
                        return Err("实际提交的替换词不准确".into());
                    }
                } // marked 阶段不读取 string 或 control.stringValue。
            } else if actual.is_some() {
                return Err("正文历史阶段没有正常辞任替换客户".into());
            }
            if matches!(stage, 1 | 2) && !step(&current, || bar.probe_found_feedback())? {
                return Err("真实导航尚未完成原匹配可见定位".into());
            }
            if stage == 3 {
                let status = surface
                    .controller
                    .ivars()
                    .status_label
                    .get()
                    .ok_or("拒绝缺少真实状态控件")?;
                let value = step(&current, || status.stringValue())?;
                if !step(&current, || {
                    value.isEqualToString(ns_string!("请先完成当前输入法组合，再执行查找或替换"))
                })? {
                    return Err("实际替换按钮没有报告原组合输入拒绝".into());
                }
            }
            drop(fields);
            if !current() {
                return Err("归还字段临时值后原查找票已失效".into());
            }
            let document = editor.document().borrow();
            if document.byte_len() != source.len()
                || document.utf16_len() != source.encode_utf16().count()
                || document.selection() != selection
                || document.is_dirty() != dirty
                || document.can_undo() != undo
                || document.can_redo() != redo
            {
                return Err("查找组合的准确源、选择或历史状态不符".into());
            }
            for (index, expected) in source.encode_utf16().enumerate() {
                if document
                    .utf16_code_unit_at(index)
                    .map_err(|cause| cause.to_string())?
                    != expected
                {
                    return Err("查找替换改动了预声明范围之外的固定源".into());
                }
            } // 只核这张固定小表的每个真实 unit，不建立实际正文副本。
            document
                .current_revision()
                .map_err(|cause| cause.to_string())
        })?;
        if matches!(stage, 1..=3 | 5) && revision != facts.baseline
            || matches!(stage, 6 | 7) && Some(revision) != facts.after
        {
            return Err("查找组合 Undo/Redo 没有恢复原完整根".into());
        }
        Self::verify_conflict_file(
            &self.path,
            if stage == 7 {
                FIND_REPLACEMENT_AFTER
            } else {
                FIND_REPLACEMENT_BEFORE
            },
        )?;
        if stage == 4 {
            let mut updated = facts.clone();
            updated.after = Some(revision); // 准确字节、范围和唯一 Undo 全部已核后，才保存真实已发布根供 Redo/保存比较。
            updated.after_selection = Some(probe.first);
            self.facts.replace(Some(updated));
        }
        Ok(())
    }

    /// 后继阶段仅执行下一实际动作；没有成功返回值替代下一事件对正文、候选和历史的认证。
    pub(crate) fn advance_find_replacement_composition(&self, stage: u8) -> Result<(), String> {
        let facts = self.facts()?;
        let mut probe = self
            .find_replacement
            .borrow_mut()
            .take()
            .ok_or("查找组合原票缺失")?;
        let result = (|| {
            self.verify_find_replacement_stage(&facts, &probe, stage)?;
            match stage {
                1 => self.click_find_replacement_button(
                    &facts,
                    &mut probe,
                    MarkdownFindAction::Previous,
                ),
                2 => self.click_find_replacement_button(
                    &facts,
                    &mut probe,
                    MarkdownFindAction::ReplaceNext,
                ),
                3 => {
                    with_active_native_markdown_surface(|surface| {
                        let current = || {
                            self.find_replacement_current(surface, &facts, &probe, facts.generation)
                        };
                        let actual = step(&current, || probe.field_editor.load())?
                            .ok_or("原替换 editor 已释放")?;
                        let value = step(&current, || NSString::from_str("中文"))?;
                        let next = probe.intent.checked_add(1).ok_or("替换确认意图溢出")?;
                        // SAFETY: 原字段仍持有0..1真实marked；用公开insert确认，不手工unmark或改写control值。
                        unsafe {
                            ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual)
                                .insertText_replacementRange(&value, NSRange::new(0, 1));
                        }
                        if !self.current(surface, &facts, facts.generation)
                            || surface.controller.find_input_intent() != next
                        {
                            return Err("真实替换词确认没有产生预声明后态".into());
                        }
                        drop((value, actual));
                        probe.intent = next;
                        Ok(())
                    })?;
                    self.click_find_replacement_button(
                        &facts,
                        &mut probe,
                        MarkdownFindAction::ReplaceNext,
                    )
                }
                4 => {
                    with_active_native_markdown_surface(|surface| {
                        let generation = facts.generation.checked_add(1).ok_or("替换代次溢出")?;
                        let current =
                            || self.find_replacement_current(surface, &facts, &probe, generation);
                        let view = &surface
                            .editor
                            .as_ref()
                            .ok_or("原正文缺失")?
                            .backend
                            .text_view;
                        if !current() || !surface.panel.makeFirstResponder(Some(&**view)) {
                            return Err("提交后替换字段拒绝正常辞任".into());
                        }
                        let after = || {
                            self.current(surface, &facts, generation)
                                && surface.controller.find_input_intent() == probe.intent
                        };
                        let bar = probe.bar.upgrade().ok_or("原查找栏已释放")?;
                        let fields = step(&after, || bar.fields())?;
                        if step(&after, || fields[1].currentEditor())?.is_some() {
                            return Err("正常辞任没有结束原替换客户".into());
                        } // 原路由应在实际成功交接时变化，不能用旧路由检查拒绝合法辞任。
                        Ok(())
                    })?;
                    probe.route = None; // 真实辞任成功后才结束字段路由票；正文 Undo 仍保原栏、Doc 和准确意图。
                    self.click_toolbar_button(1, 22)
                }
                5 => self.click_toolbar_button(2, 23),
                6 => with_active_native_markdown_surface(|surface| {
                    let generation = facts.generation.checked_add(3).ok_or("Redo代次溢出")?;
                    if !self.find_replacement_current(surface, &facts, &probe, generation) {
                        return Err("查找组合保存前原票已撤销".into());
                    }
                    surface.save_current_document() // 只用正常保存，不直接写测试期望到磁盘。
                }),
                7 => {
                    println!(
                        "native_find_replacement_composition actual_next_previous_buttons=true marked_preserved=true actual_replace_rejected=true actual_commit_replace=true canonical_undo_redo_exact=true production_save_exact=true physical_ime=false formal=false"
                    );
                    Ok(())
                }
                _ => Err("未知查找组合动作阶段".into()),
            }
        })();
        self.find_replacement.replace(Some(probe)); // 错误也归还原弱状态，仍走唯一真实 Return/strict-close。
        result
    }

    /// 已知原NSAlert装配完成后才登记一次modal-mode动作；不查找或遍历系统窗口子树。
    pub(in crate::native_surface) fn observe_conflict_modal_ready(
        alert: &NSAlert,
        window: &objc2_app_kit::NSWindow,
        field: Option<&NSTextField>,
    ) {
        let operation = CONFLICT_OPERATION.with_borrow(std::rc::Weak::upgrade);
        let Some(operation) = operation.filter(|operation| {
            operation.active.get()
                && !operation.delivered.get()
                && operation.action != ConflictProbeAction::ReturnOriginal
        }) else {
            return; // 普通产品对话框和未武装验收不产生回调或输入。
        };
        if operation.modal_seen.replace(true) {
            *operation.modal_result.borrow_mut() =
                Some(Err("同一合成冲突操作重复打开对话框".into()));
            return;
        }
        let expected_copy = operation.action == ConflictProbeAction::CopyDraft;
        let titles = if expected_copy {
            ("保留本地草稿", "另存为新笔记")
        } else {
            ("重新加载磁盘版本？", "放弃草稿并重新加载")
        };
        let buttons = alert.buttons();
        if buttons.count() != 2 {
            *operation.modal_result.borrow_mut() =
                Some(Err("实际冲突对话框缺少原确认/取消按钮".into()));
            return; // 不点击身份未知的按钮，更不能为验收伪造NSAlert返回码。
        }
        let confirm = buttons.objectAtIndex(0);
        let cancel = buttons.objectAtIndex(1);
        let prompt_matches = operation.original.current()
            && alert
                .messageText()
                .isEqualToString(&NSString::from_str(titles.0))
            && confirm
                .title()
                .isEqualToString(&NSString::from_str(titles.1))
            && cancel.title().isEqualToString(ns_string!("取消"))
            && field.is_some() == expected_copy;
        let mtm = window.mtm();
        let controls = MainThreadBound::new(
            (
                Weak::from(window),
                Weak::from_retained(&confirm),
                Weak::from_retained(&cancel),
                field.map(Weak::from),
                Rc::clone(&operation),
            ),
            mtm,
        );
        let callback = block2::RcBlock::new(move || {
            let mtm = MainThreadMarker::new().expect("原modal run loop位于主线程");
            let (window, confirm, cancel, field, operation) = controls.get(mtm);
            if !operation.active.get() {
                return; // 失败清理已撤权的旧block不能点击另一张后来出现的对话框。
            }
            let result = (|| {
                if !prompt_matches || !operation.original.current() {
                    return Err("真实冲突对话框文案、输入类型或原票不符".into());
                }
                let window = window.load().ok_or("原冲突对话框已经释放")?;
                let actual = NSApplication::sharedApplication(mtm)
                    .modalWindow()
                    .ok_or("原NSAlert尚未进入真实modal会话")?;
                if !std::ptr::eq(&*actual, &*window) {
                    return Err("modal输入目标不是本次实际NSAlert".into());
                }
                drop(actual);
                if let Some(field) = field {
                    // 名称仍由原独占字段/provider处理；真实输入先替换默认标题，再由原确认按钮辞任。
                    let field = field.load().ok_or("原名称字段已经释放")?;
                    if !window.makeFirstResponder(Some(&field)) {
                        return Err("原名称字段拒绝真实焦点".into());
                    }
                    let editor = field
                        .currentEditor()
                        .ok_or("原名称字段没有真实field editor")?
                        .downcast::<NSTextView>()
                        .map_err(|_| "原名称字段editor类型不同")?;
                    let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
                    if input.hasMarkedText() {
                        return Err("合成名称字段意外包含未完成组合".into());
                    }
                    let length = editor.string().length();
                    // SAFETY: 标准NSTextInputClient只替换这个已明确武装、有限名称字段的完整默认值。
                    unsafe {
                        input.insertText_replacementRange(
                            &NSString::from_str(CONFLICT_COPY_TITLE),
                            NSRange::new(0, length),
                        );
                    }
                    if !editor
                        .string()
                        .isEqualToString(&NSString::from_str(CONFLICT_COPY_TITLE))
                    {
                        return Err("真实名称输入没有产生预声明的有限标题".into());
                    }
                }
                Ok(())
            })();
            let use_cancel =
                result.is_err() || operation.action == ConflictProbeAction::CancelReload;
            *operation.modal_result.borrow_mut() = Some(result);
            let button = if use_cancel { cancel } else { confirm };
            if let Some(button) = button.load() {
                // SAFETY: 只点击原alert已登记按钮；保留NSAlert自有target/action与实际返回码，失败也走真实取消。
                unsafe { button.performClick(None) };
            } else {
                *operation.modal_result.borrow_mut() = Some(Err("原对话框按钮已释放".into()));
            }
            let completion = MainThreadBound::new(Rc::clone(operation), mtm);
            enqueue_native_markdown_main_task(move || {
                let mtm = MainThreadMarker::new().expect("原modal栈退回后沿主队列交付");
                let operation = completion.get(mtm);
                if operation.action == ConflictProbeAction::CancelReload
                    || operation
                        .modal_result
                        .borrow()
                        .as_ref()
                        .is_some_and(Result::is_err)
                {
                    operation.deliver();
                } // 复制/重载成功只能等下面真实安装通知；队列退回不足以证明旧Doc释放。
            });
        });
        let run_loop = objc2_core_foundation::CFRunLoop::main().expect("验收已有AppKit主run loop");
        // SAFETY: 框架导出的NSString模式常量可无拷贝桥接为CFString；主run loop复制一次heap block。
        // block仅借主线程封装的Weak和原合成票，不在serial dispatch block中等待后续dispatch任务。
        unsafe {
            let mode: &CFString = objc2_app_kit::NSModalPanelRunLoopMode.as_ref();
            run_loop.perform_block(Some(mode.as_ref()), Some(&callback));
        }
        run_loop.wake_up();
    }

    /// 原生产finish_note_switch成功后调用；同时要求旧Doc已真实消失和新路径已安装。
    pub(in crate::native_surface) fn observe_conflict_note_installed() {
        let operation = CONFLICT_OPERATION.with_borrow(std::rc::Weak::upgrade);
        let Some(operation) = operation.filter(|operation| {
            operation.active.get()
                && !operation.delivered.get()
                && operation.action != ConflictProbeAction::CancelReload
        }) else {
            return;
        };
        let controller = operation.controller.load();
        let installed = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active.as_ref().is_some_and(|surface| {
                controller
                    .as_ref()
                    .is_some_and(|controller| std::ptr::eq(&**controller, &*surface.controller))
                    && surface.pending_note_switch.is_none()
                    && surface.current_note_relative_path.as_deref() == Some(operation.destination)
                    && surface.editor.is_some()
                    && operation.original.document.strong_count() == 0
            })
        });
        drop(controller); // 强Native临时值先归还，后继只投原driver事件。
        if installed {
            operation.deliver();
        }
    }

    /// 每个真实动作只武装一次原票；上一个终点被消费后才允许下一个动作。
    fn arm_conflict_operation(
        &self,
        action: ConflictProbeAction,
        destination: &'static str,
        next_stage: i16,
    ) -> Result<(), String> {
        if self.conflict_operation.borrow().is_some() {
            return Err("上一冲突操作的实际终点尚未消费".into());
        }
        let (original, controller) = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("冲突动作没有实际原文稿")?;
            let path = surface
                .current_note_relative_path
                .as_deref()
                .ok_or("冲突动作没有实际路径")?;
            let original = NativeMarkdownCurrentNoteIntent::for_document(
                &surface.controller,
                &surface.panel,
                editor.document(),
                path,
            );
            Ok((original, Weak::from_retained(&surface.controller)))
        })?;
        let operation = Rc::new(ConflictProbeOperation {
            controller,
            original,
            action,
            destination,
            terminal: Rc::clone(&self.terminal),
            next_stage,
            active: Cell::new(true),
            modal_seen: Cell::new(false),
            delivered: Cell::new(false),
            modal_result: RefCell::new(None),
        });
        CONFLICT_OPERATION.with_borrow_mut(|slot| *slot = Rc::downgrade(&operation));
        self.conflict_operation.replace(Some(operation));
        Ok(())
    }

    /// 消费真实完成收据，不能把新路径的某个后来状态补签为旧操作成功。
    fn consume_conflict_operation(&self, stage: i16) -> Result<(), String> {
        let operation = self
            .conflict_operation
            .borrow_mut()
            .take()
            .ok_or("缺少原冲突操作收据")?;
        operation.active.set(false);
        CONFLICT_OPERATION.with_borrow_mut(|slot| *slot = std::rc::Weak::new());
        if operation.next_stage != stage || !operation.delivered.get() {
            return Err("冲突实际终点不是预声明阶段".into());
        }
        if operation.action != ConflictProbeAction::ReturnOriginal {
            operation
                .modal_result
                .borrow()
                .clone()
                .ok_or("真实NSAlert输入没有执行")??;
        }
        Ok(())
    }

    /// 517只使用原字段历史已经正式保存的短夹具；独立写磁盘后必须走真实保存冲突。
    pub(crate) fn begin_conflict_bar_case(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let conflict = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("冲突前没有原文稿")?;
            let document = editor.document().borrow();
            let snapshot = ConflictDocumentSnapshot::capture(&document)?;
            if self.case != NativeMarkdownTableAppendCase::ToolbarFieldHistory
                || !WeakMarkdownDocument::ptr_eq(&facts.document, &Rc::downgrade(editor.document()))
                || Some(snapshot.revision) != facts.after
                || Some(snapshot.selection) != facts.after_selection
                || snapshot.dirty
                || !snapshot.undo
                || snapshot.redo
            {
                return Err("原字段历史与生产保存没有全部通过，不能开始冲突用例".into());
            }
            Self::verify_conflict_text(&document, CONFLICT_DRAFT)?;
            drop(document);
            let original = NativeMarkdownCurrentNoteIntent::for_document(
                &surface.controller,
                &surface.panel,
                editor.document(),
                "工作负载.md",
            );
            let bar = surface.controller.ivars().conflict_bar.get().unwrap();
            Ok(ConflictProbeFacts {
                original,
                publication: surface.controller.conflict_publication(),
                snapshot,
                current_document: Rc::downgrade(editor.document()),
                current_path: "工作负载.md",
                current_text: CONFLICT_DRAFT,
                original_disk_text: CONFLICT_DRAFT,
                bar: Weak::from(bar.probe_controls().0),
                completed: false,
            })
        })?;
        self.conflict.replace(Some(conflict)); // 先记有限夹具事实，失败也只能对这一自有原票做清理。
        std::fs::write(&self.path, CONFLICT_EXTERNAL).map_err(|cause| cause.to_string())?;
        self.conflict
            .borrow_mut()
            .as_mut()
            .unwrap()
            .original_disk_text = CONFLICT_EXTERNAL;
        with_active_native_markdown_surface(|surface| {
            if surface.save_current_document().is_ok() {
                return Err("真实外部覆写没有触发生产保存冲突".into());
            }
            self.conflict.borrow_mut().as_mut().unwrap().publication =
                surface.controller.conflict_publication();
            Ok(())
        })?;
        self.verify_conflict_source_unchanged()?;
        self.verify_conflict_geometry()?;
        Self::verify_conflict_file(&self.path, CONFLICT_EXTERNAL)?;
        self.arm_conflict_operation(ConflictProbeAction::CancelReload, "工作负载.md", 518)?;
        self.click_conflict_button(4)
    }

    /// 518只接受真实取消；原字节/选择/Undo不动后再点击原另存按钮。
    pub(crate) fn verify_conflict_cancel_and_copy(&self) -> Result<(), String> {
        self.consume_conflict_operation(518)?;
        self.verify_conflict_source_unchanged()?;
        Self::verify_conflict_file(&self.path, CONFLICT_EXTERNAL)?;
        let copy = self.path.parent().unwrap().join(CONFLICT_COPY_PATH);
        match std::fs::metadata(&copy) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err("取消重载意外产生了另存文件".into()),
            Err(error) => return Err(error.to_string()),
        }
        println!(
            "native_conflict_cancel actual_alert_cancel=true disk_unchanged=true canonical_selection_history_unchanged=true formal=false"
        );
        self.arm_conflict_operation(ConflictProbeAction::CopyDraft, CONFLICT_COPY_PATH, 519)?;
        self.click_conflict_button(5)
    }

    /// 519由真实新笔记安装交付；副本必须保存原草稿，外部版本不能被覆盖。
    pub(crate) fn verify_conflict_copy_and_return(&self) -> Result<(), String> {
        self.consume_conflict_operation(519)?;
        let original = self.conflict.borrow().as_ref().unwrap().original.clone();
        if original.document.strong_count() != 0 {
            return Err("另存安装新笔记后原Doc仍活".into());
        }
        self.accept_conflict_document(CONFLICT_COPY_PATH, CONFLICT_DRAFT)?;
        Self::verify_conflict_file(&self.path, CONFLICT_EXTERNAL)?;
        Self::verify_conflict_file(
            &self.path.parent().unwrap().join(CONFLICT_COPY_PATH),
            CONFLICT_DRAFT,
        )?;
        println!(
            "native_conflict_copy actual_name_field_input=true actual_alert_confirm=true exact_copy=true original_external_bytes_preserved=true original_document_released=true formal=false"
        );
        self.arm_conflict_operation(ConflictProbeAction::ReturnOriginal, "工作负载.md", 520)?;
        switch_native_markdown_note("工作负载.md")
    }

    /// 520已真实回到同名A2；旧A票必须因Doc身份失效，随后才允许这次A2真实重载。
    pub(crate) fn verify_conflict_aba_and_reload(&self) -> Result<(), String> {
        self.consume_conflict_operation(520)?;
        let (original, publication, copy_document) = {
            let conflict = self.conflict.borrow();
            let conflict = conflict.as_ref().unwrap();
            (
                conflict.original.clone(),
                conflict.publication,
                conflict.current_document.clone(),
            )
        };
        if copy_document.strong_count() != 0 {
            return Err("从副本切回原路径后副本Doc仍活".into());
        }
        self.accept_conflict_document("工作负载.md", CONFLICT_EXTERNAL)?;
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().unwrap();
            if original.relative_path != surface.current_note_relative_path.as_deref().unwrap()
                || WeakMarkdownDocument::ptr_eq(
                    &original.document,
                    &Rc::downgrade(editor.document()),
                )
                || original.document.strong_count() != 0
            {
                return Err("ABA没有采用同一路径与新的实际Doc身份".into());
            }
            Ok(())
        })?;
        if original.current() {
            return Err("同名A2意外恢复了旧A操作票".into());
        }
        original.perform_resolution(4, publication)?; // 直接消费原旧票，不能从现在A2重新捕获权限。
        self.verify_conflict_source_unchanged()?;
        println!(
            "native_conflict_aba same_relative_path=true different_document_identity=true old_document_released=true old_ticket_rejected=true formal=false"
        );
        std::fs::write(&self.path, CONFLICT_RELOADED).map_err(|cause| cause.to_string())?;
        self.conflict
            .borrow_mut()
            .as_mut()
            .unwrap()
            .original_disk_text = CONFLICT_RELOADED;
        with_active_native_markdown_surface(|surface| {
            if surface.save_current_document().is_ok() {
                return Err("第二次真实外部修改没有触发生产保存冲突".into());
            }
            Ok(())
        })?;
        self.arm_conflict_operation(ConflictProbeAction::ConfirmReload, "工作负载.md", 521)?;
        self.click_conflict_button(4)
    }

    /// 521只在真实重载安装后检查；最后的原生产Return仍须再通过保存和完整strict释放。
    pub(crate) fn verify_conflict_reload(&self) -> Result<(), String> {
        self.consume_conflict_operation(521)?;
        let previous = self
            .conflict
            .borrow()
            .as_ref()
            .unwrap()
            .current_document
            .clone();
        if previous.strong_count() != 0 {
            return Err("真实重载后被放弃的A2 Doc仍活".into());
        }
        self.accept_conflict_document("工作负载.md", CONFLICT_RELOADED)?;
        Self::verify_conflict_file(&self.path, CONFLICT_RELOADED)?;
        Self::verify_conflict_file(
            &self.path.parent().unwrap().join(CONFLICT_COPY_PATH),
            CONFLICT_DRAFT,
        )?;
        with_active_native_markdown_surface(|surface| {
            if !surface
                .controller
                .ivars()
                .conflict_bar
                .get()
                .unwrap()
                .probe_controls()
                .0
                .isHidden()
            {
                return Err("成功重载后旧冲突条仍显示".into());
            }
            surface.save_current_document()
        })?;
        self.verify_conflict_source_unchanged()?;
        self.conflict.borrow_mut().as_mut().unwrap().completed = true;
        println!(
            "native_conflict_reload actual_alert_confirm=true actual_handoff=true exact_external_source=true old_document_released=true formal=false"
        );
        Ok(())
    }

    /// 只按固定合成文本逐UTF16比较真实canonical；不构造实际正文副本。
    fn verify_conflict_text(document: &MarkdownDocumentBuffer, text: &str) -> Result<(), String> {
        if document.byte_len() != text.len() || document.utf16_len() != text.encode_utf16().count()
        {
            return Err("冲突处理后的实际正文长度不同".into());
        }
        for (offset, unit) in text.encode_utf16().enumerate() {
            if document
                .utf16_code_unit_at(offset)
                .map_err(|cause| cause.to_string())?
                != unit
            {
                return Err("冲突处理改变了预声明范围以外的实际源".into());
            }
        }
        Ok(())
    }

    /// 只读自有短文件的明确字节与EOF；FD在返回前关闭，不成为产品资源观察者。
    fn verify_conflict_file(path: &Path, text: &str) -> Result<(), String> {
        let mut file = std::fs::File::open(path).map_err(|cause| cause.to_string())?;
        let mut bytes = [0_u8; 256];
        std::io::Read::read_exact(&mut file, &mut bytes[..text.len()])
            .map_err(|cause| cause.to_string())?;
        let mut tail = [0_u8; 1];
        if &bytes[..text.len()] != text.as_bytes()
            || std::io::Read::read(&mut file, &mut tail).map_err(|cause| cause.to_string())? != 0
        {
            return Err("冲突流程的磁盘字节或EOF不符".into());
        }
        Ok(())
    }

    /// 认证真实新Doc再更新观察事实；没有重签任何生产操作票。
    fn accept_conflict_document(
        &self,
        path: &'static str,
        text: &'static str,
    ) -> Result<(), String> {
        let (document, snapshot) = with_active_native_markdown_surface(|surface| {
            if surface.current_note_relative_path.as_deref() != Some(path)
                || surface.pending_note_switch.is_some()
            {
                return Err("真实笔记安装的目标或完成状态不符".into());
            }
            let editor = surface.editor.as_ref().ok_or("真实安装没有原生编辑器")?;
            let document = editor.document().borrow();
            Self::verify_conflict_text(&document, text)?;
            let snapshot = ConflictDocumentSnapshot::capture(&document)?;
            if snapshot.dirty || snapshot.undo || snapshot.redo {
                return Err("新安装Doc继承了旧草稿或历史".into());
            }
            Ok((Rc::downgrade(editor.document()), snapshot))
        })?;
        let mut conflict = self.conflict.borrow_mut();
        let conflict = conflict.as_mut().unwrap();
        conflict.current_document = document;
        conflict.snapshot = snapshot;
        conflict.current_path = path;
        conflict.current_text = text;
        Ok(())
    }

    /// 取消、Find和缩放都必须保持原根、选区、dirty与Undo/Redo；比较真实正文Native选区。
    fn verify_conflict_source_unchanged(&self) -> Result<(), String> {
        let (document, snapshot, path, text) = {
            let conflict = self.conflict.borrow();
            let conflict = conflict.as_ref().unwrap();
            (
                conflict.current_document.clone(),
                conflict.snapshot,
                conflict.current_path,
                conflict.current_text,
            )
        };
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("冲突操作丢失原编辑器")?;
            if surface.current_note_relative_path.as_deref() != Some(path)
                || !WeakMarkdownDocument::ptr_eq(&document, &Rc::downgrade(editor.document()))
                || ConflictDocumentSnapshot::capture(&editor.document().borrow())? != snapshot
            {
                return Err("冲突取消或只读交互改变了原Doc、选择或历史".into());
            }
            Self::verify_conflict_text(&editor.document().borrow(), text)?;
            let start = snapshot
                .selection
                .anchor_utf16
                .min(snapshot.selection.head_utf16);
            let end = snapshot
                .selection
                .anchor_utf16
                .max(snapshot.selection.head_utf16);
            if editor
                .backend
                .text_view
                .native_ordered_selection_range()
                .map_err(|cause| cause.to_string())?
                != NSRange::new(start, end - start)
            {
                return Err("冲突交互改变了真实正文选区".into());
            }
            Ok(())
        })
    }

    /// 始终点击这条实际安装的按钮；不调用perform_resolution代替用户入口。
    fn click_conflict_button(&self, tag: isize) -> Result<(), String> {
        let button = with_active_native_markdown_surface(|surface| {
            let (_, message, buttons) = surface
                .controller
                .ivars()
                .conflict_bar
                .get()
                .unwrap()
                .probe_controls();
            if !message.stringValue().isEqualToString(ns_string!(
                "检测到磁盘版本变化，本地草稿已保留。请选择处理方式。"
            )) {
                return Err("真实保存结果没有发布准确中文冲突提示".into());
            }
            let button = &buttons[if tag == 4 { 0 } else { 1 }];
            if button.tag() != tag || !button.isEnabled() || !button.refusesFirstResponder() {
                return Err("实际冲突处理按钮不可用或夺取输入焦点".into());
            }
            Ok(button.clone())
        })?;
        // SAFETY: 原按钮的target/action由生产装配；释放Active后才让它捕获原票并入原队列。
        unsafe { button.performClick(None) };
        Ok(())
    }

    /// 最窄窗口叠加真实Find/Replace后，只比较实际控件frame；不调用生产几何函数签发结果。
    fn verify_conflict_geometry(&self) -> Result<(), String> {
        let panel = with_active_native_markdown_surface(|surface| Ok(surface.panel.clone()))?;
        let original = panel
            .contentView()
            .ok_or("冲突窗口缺少内容根")?
            .frame()
            .size;
        show_native_markdown_find_bar(true)?;
        let checked = (|| {
            panel.setContentSize(NSSize::new(900.0, 600.0)); // 原windowDidResize负责布局，不由验收补调用布局实现。
            panel.displayIfNeeded();
            self.verify_conflict_source_unchanged()?;
            with_active_native_markdown_surface(|surface| {
                let editor = surface.editor.as_ref().ok_or("最窄窗口丢失原文稿")?;
                let content = surface
                    .panel
                    .contentView()
                    .ok_or("最窄窗口缺少实际内容根")?;
                let size = content.frame().size;
                if size != NSSize::new(900.0, 600.0) {
                    return Err("窗口没有采用原最窄内容尺寸".into());
                }
                let (root, message, buttons) = surface
                    .controller
                    .ivars()
                    .conflict_bar
                    .get()
                    .unwrap()
                    .probe_controls();
                let frame = root.frame();
                let old_root = self.conflict.borrow().as_ref().unwrap().bar.clone();
                let actual_root = old_root.load().ok_or("原冲突条已释放")?;
                if !std::ptr::eq(&*actual_root, root)
                    || root.isHidden()
                    || !root
                        .accessibilityLabel()
                        .is_some_and(|label| label.isEqualToString(ns_string!("笔记冲突处理")))
                    || !message.stringValue().isEqualToString(ns_string!(
                        "检测到磁盘版本变化，本地草稿已保留。请选择处理方式。"
                    ))
                {
                    return Err("真实冲突条身份、中文提示或AX不符".into());
                }
                drop(actual_root);
                let message_frame = message.frame();
                let mut parts = vec![message_frame];
                for (button, title) in buttons
                    .iter()
                    .zip(["重新加载磁盘版本", "将本地草稿另存为新笔记"])
                {
                    if !button.title().isEqualToString(&NSString::from_str(title))
                        || !button
                            .accessibilityLabel()
                            .is_some_and(|label| label.isEqualToString(&NSString::from_str(title)))
                        || !button.isEnabled()
                        || !button.refusesFirstResponder()
                        || button.isHidden()
                    {
                        return Err("原两个直达按钮的中文标题、可用性或焦点规则不同".into());
                    }
                    parts.push(button.frame());
                }
                for (index, part) in parts.iter().enumerate() {
                    if part.origin.x < 0.0
                        || part.origin.y < 0.0
                        || part.size.width <= 0.0
                        || part.size.height <= 0.0
                        || part.origin.x + part.size.width > frame.size.width
                        || part.origin.y + part.size.height > frame.size.height
                        || parts[index + 1..]
                            .iter()
                            .any(|other| NSIntersectsRect(*part, *other))
                    {
                        return Err("最窄窗口冲突文字与两个按钮裁切或相互重叠".into());
                    }
                }
                let find = editor
                    .find_bar
                    .borrow()
                    .clone()
                    .ok_or("冲突验收没有真实Find栏")?;
                let find = find
                    .visible_frame_for_probe()
                    .ok_or("最窄窗口隐藏了实际Find栏")?;
                let body = editor.backend.scroll_view().frame();
                let footers = surface
                    .controller
                    .ivars()
                    .footer_labels
                    .get()
                    .unwrap()
                    .each_ref()
                    .map(|label| label.frame());
                let status = surface.controller.ivars().status_label.get().unwrap();
                let feedback = (!status.isHidden()).then(|| status.frame());
                let controls = surface.controller.ivars().workspace_controls.get().unwrap();
                let format = controls[NATIVE_MARKDOWN_TOOLBAR_START + 5]
                    .enclosingScrollView()
                    .ok_or("实际格式栏缺少滚动视口")?
                    .frame();
                let mut regions = vec![frame, find, body, format];
                regions.extend(footers);
                regions.extend(feedback);
                for (index, region) in regions.iter().enumerate() {
                    if region.origin.x < 0.0
                        || region.origin.y < 0.0
                        || region.size.width <= 0.0
                        || region.size.height <= 0.0
                        || region.origin.x + region.size.width > size.width
                        || region.origin.y + region.size.height > size.height
                        || regions[index + 1..]
                            .iter()
                            .any(|other| NSIntersectsRect(*region, *other))
                    {
                        return Err("最窄窗口的冲突条、Find、正文、反馈或底栏互相遮挡".into());
                    }
                }
                Ok(())
            })
        })();
        let hidden = perform_native_markdown_find_action(MarkdownFindAction::Hide);
        panel.setContentSize(original);
        panel.displayIfNeeded();
        drop(panel); // 不跨对话框或最终关闭保留原窗口强引用。
        checked?;
        hidden?;
        self.verify_conflict_source_unchanged()?;
        println!(
            "native_conflict_geometry minimum=900x600 actual_find_replace=true actual_native_frames_nonoverlapping=true chinese_buttons=true source_selection_history_unchanged=true formal=false"
        );
        Ok(())
    }

    fn current(
        &self,
        surface: &NativeMarkdownWindowSurface,
        facts: &AppendFacts,
        generation: u64,
    ) -> bool {
        native_markdown_controller_has_foreground_authority(&surface.controller)
            && std::ptr::from_ref(&*surface.controller).addr() == facts.owner
            && surface.editor.as_ref().is_some_and(|editor| {
                !editor
                    .backend
                    .text_view
                    .ivars()
                    .textkit_graph_teardown
                    .get()
                    && editor.backend.text_storage.presentation_mode()
                        == MarkdownPresentationMode::InlinePreview
                    && editor
                        .backend
                        .text_storage
                        .ivars()
                        .table_projection_width_key
                        .get()
                        == facts.width
                    && WeakMarkdownDocument::ptr_eq(
                        &Rc::downgrade(editor.document()),
                        &facts.document,
                    )
                    && editor.document().borrow().generation() == generation
            })
    }

    pub(crate) fn verify_open(&self) -> Result<(), String> {
        let facts = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("小表没有真实编辑器")?;
            let document = editor.document().borrow();
            let facts = AppendFacts {
                owner: std::ptr::from_ref(&*surface.controller).addr(),
                document: Rc::downgrade(editor.document()),
                generation: document.generation(),
                width: editor
                    .backend
                    .text_storage
                    .ivars()
                    .table_projection_width_key
                    .get(),
                baseline: document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                selection: document.selection(),
                after: None,
                after_selection: None,
            };
            drop(document); // 显示前结束canonical借用，不以整篇ensure取得几何。
            let _payload =
                PayloadUseGuard::enter(&editor.backend.text_view.ivars().payload_use_depth);
            let current = || self.current(surface, &facts, facts.generation);
            step(&current, || surface.panel.displayIfNeeded())?;
            let application = step(&current, || {
                NSApplication::sharedApplication(surface.panel.mtm())
            })?;
            step(&current, || application.updateWindows())?;
            drop(application);
            if !current() {
                return Err("小表首次显示结束后已撤权".into());
            }
            Ok(facts)
        })?;
        if self.case == NativeMarkdownTableAppendCase::ToolbarFormatSelection {
            self.verify_toolbar_resize(&facts)?;
        }
        self.facts.replace(Some(facts));
        Ok(())
    }

    /// 502只请求真实末格激活；没有原终点时不读取共享field editor，也不输入或触发Next。
    pub(crate) fn begin_append(&self) -> Result<(), String> {
        let facts = self.facts()?;
        let receipt = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            if !current() {
                return Err("小表激活入口已撤权".into());
            }
            let editor = surface.editor.as_ref().ok_or("小表缺少编辑器")?;
            let view = &editor.backend.text_view;
            let payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            // 槽内只复制纯Rc；cell属性retain及cache最终drop都在Ref借用结束后进行。
            let cache = view
                .ivars()
                .visible_table_layout_cache
                .borrow()
                .as_ref()
                .map(Rc::clone);
            let cell = cache
                .as_ref()
                .and_then(|cache| {
                    cache
                        .tables
                        .iter()
                        .find(|table| table.table_utf16_start == 0)
                })
                .and_then(|table| {
                    table.cells.iter().find(|cell| {
                        cell.coordinate.row_index == 1 && cell.coordinate.column_index == 1
                    })
                })
                .cloned();
            drop(cache);
            if !current() {
                return Err("末格几何属性复制或析构后已撤权".into());
            }
            let cell = cell.ok_or("小表真实末格几何尚不可用")?;
            let coordinate = cell.coordinate;
            let receipt = view.probe_prepare_table_cell_receipt(
                editor.document(),
                coordinate,
                cell_activation_terminal,
            )?;
            // 两调用同栈相邻，不插Native getter；原预声明只可附到这次生产激活请求。
            if !view.probe_begin_table_cell_editing(cell, Rc::clone(&receipt)) {
                return Err("小表未接受原单格激活请求".into());
            }
            if receipt.original_generation() != facts.generation
                || receipt.original_coordinate() != coordinate
            {
                return Err("单格激活收据不属于原末格".into());
            }
            drop(payload); // 栈内Native claims/guard结束后仍以同一原票证明异步请求归属。
            if !current() || !receipt.current_for(view) {
                return Err("单格激活请求返回后已撤权".into());
            }
            Ok(receipt)
        })?;
        let previous = self.activation_receipt.replace(Some(receipt));
        drop(previous);
        Ok(())
    }

    /// 507只消费原Activated收据；此后实际输入，再同栈预声明原追加收据并立即Next。
    pub(crate) fn finish_cell_activation_and_begin_append(&self) -> Result<(), String> {
        let activation = self
            .activation_receipt
            .borrow()
            .as_ref()
            .map(Rc::clone)
            .ok_or("追加回归缺少原单格激活收据")?;
        if activation.outcome() != Some(NativeMarkdownTableCellOutcome::Activated) {
            return Err(format!("原单格未完成真实激活：{:?}", activation.outcome()));
        }
        let mut facts = self.facts()?;
        if activation.original_generation() != facts.generation
            || activation.original_coordinate().row_index != 1
            || activation.original_coordinate().column_index != 1
        {
            return Err("单格激活终点不是原g0末格".into());
        }
        let (receipt, selection) = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            if !current() {
                return Err("单格激活终点的原窗口或正文已撤权".into());
            }
            let editor = surface.editor.as_ref().ok_or("单格激活后编辑器缺失")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            let activation_current = || current() && activation.current_for(view);
            if !activation_current() {
                return Err("迟到的单格激活终点没有原输入权限".into());
            }
            let original_cell = view
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .map(|state| (state.coordinate, state.source_utf16_range.clone()))
                .ok_or("单格激活终点没有原活动格")?;
            if original_cell.0 != activation.original_coordinate() {
                return Err("单格激活终点与实际字段坐标不符".into());
            }
            (|| -> Result<(), String> {
                let field = step(&activation_current, || view.existing_table_cell_editor())?
                    .ok_or("末格没有实际字段")?;
                let native = step(&activation_current, || field.currentEditor())?
                    .ok_or("末格没有实际field editor")?;
                let field_editor = native
                    .downcast::<NSTextView>()
                    .map_err(|_| "末格field editor类型不符")?;
                let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*field_editor);
                if step(&activation_current, || input.hasMarkedText())? {
                    return Err("合成末格存在未完成composition".into());
                }
                let value = step(&activation_current, || field_editor.string())?;
                let units = step(&activation_current, || value.length())?;
                drop(value);
                if !activation_current() || units > 256 {
                    return Err("末格输入超过固定合成预算".into());
                }
                step(&activation_current, || {
                    field_editor.setSelectedRange(NSRange::new(0, units))
                })?;
                // 预算反例只构造已知65537个ASCII草稿；不是正文副本，也不改变产品的65536准入。
                let budget_text = if self.case == NativeMarkdownTableAppendCase::Budget {
                    let ascii = "A".repeat(OVER_BUDGET_UNITS);
                    Some(step(&activation_current, || NSString::from_str(&ascii))?)
                } else {
                    None // 普通与取消反例仍输入同一个有限草稿，不分叉产品输入路径。
                };
                let text = budget_text.as_deref().unwrap_or(ns_string!("末格追加验收"));
                if !activation_current() {
                    return Err("实际输入前原单格激活权限已改变".into());
                }
                // SAFETY: 实有field editor接受本次预声明输入；它可合法推进自己的input epoch。
                // 从此不再消费旧激活票，但仍受同一原g0/owner/width约束，不能取得后继正文权限。
                step(&current, || unsafe {
                    input.insertText_replacementRange(text, NSRange::new(0, units));
                })?;
                Ok(())
            })()?; // 所有field/native string临时claim先结束，再准备原Next收据。
            if !current()
                || view
                    .ivars()
                    .active_table_cell
                    .borrow()
                    .as_ref()
                    .map(|state| (state.coordinate, state.source_utf16_range.clone()))
                    != Some(original_cell)
            {
                return Err("末格输入局部析构后原字段或正文已撤权".into());
            }
            let selection = editor.document().borrow().selection();
            let receipt =
                view.probe_prepare_table_append_receipt(editor.document(), append_terminal)?;
            view.navigate_active_table_cell(NativeMarkdownTableNavigation::Next);
            // 两者间无Native getter；收据的原gen和coordinate不可被后继当前状态替代。
            if receipt.original_generation() != facts.generation
                || receipt.original_coordinate().row_index != 1
                || receipt.original_coordinate().column_index != 1
            {
                return Err("追加收据不是本次原末格".into());
            }
            if self.case == NativeMarkdownTableAppendCase::Cancel {
                // 首Native取消前只核原收据与固定g0；若已经同步终结/提交，就不是本反例的取消窗口。
                if receipt.outcome().is_some()
                    || editor.document().borrow().generation() != facts.generation
                {
                    return Err("同步取消前原追加请求已经终结或提交".into());
                }
                step(&current, || view.cancel_active_table_cell())?;
            }
            Ok((receipt, selection))
        })?;
        // 原current闭包及Native临时claim已结束，才把首Next前捕获的纯选区写回事实。
        facts.selection = selection;
        self.facts.replace(Some(facts));
        let old = self.receipt.replace(Some(receipt));
        drop(old);
        Ok(())
    }

    /// 原 Activated 已证明格身份；两个格式场景都消费同一实际字段和原菜单票。
    pub(crate) fn verify_format(&self) -> Result<(), String> {
        let activation = self
            .activation_receipt
            .borrow()
            .as_ref()
            .map(Rc::clone)
            .ok_or("无变化格式缺少原单格激活收据")?;
        if activation.outcome() != Some(NativeMarkdownTableCellOutcome::Activated) {
            return Err("无变化格式未完成真实单格激活".into());
        }
        let facts = self.facts()?;
        let (controller, view_identity, field, selection_before) =
            with_active_native_markdown_surface(|surface| {
                let current = || self.current(surface, &facts, facts.generation);
                let editor = surface.editor.as_ref().ok_or("无变化格式缺少实际编辑器")?;
                let view = &editor.backend.text_view;
                let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
                if !current() || !activation.current_for(view) {
                    return Err("无变化格式的原激活权限已撤销".into());
                }
                let field = step(&current, || view.existing_table_cell_editor())?
                    .ok_or("原单格字段缺失")?;
                {
                    let native = step(&current, || field.currentEditor())?
                        .ok_or("原单格没有实际 field editor")?;
                    let actual = native
                        .downcast::<NSTextView>()
                        .map_err(|_| "原单格编辑器类型不符")?;
                    let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
                    if step(&current, || input.hasMarkedText())? {
                        return Err("无变化格式合成输入不应已有 composition".into());
                    }
                    let text = step(&current, || actual.string())?;
                    let selected_case = matches!(
                        self.case,
                        NativeMarkdownTableAppendCase::FormatSelection
                            | NativeMarkdownTableAppendCase::ToolbarFormatSelection
                    );
                    let original = if selected_case {
                        ns_string!("前中😀后")
                    } else {
                        ns_string!("**甲**")
                    };
                    if !step(&current, || text.isEqualToString(original))? {
                        return Err("无变化格式原字段不是固定加粗源".into());
                    }
                    drop(text);
                    if !selected_case {
                        // SAFETY: 原真实字段同步接收有限替换；该输入推进 epoch，后面不再消费旧激活票。
                        step(&current, || unsafe {
                            input.insertText_replacementRange(ns_string!("甲"), NSRange::new(0, 5));
                        })?;
                    } // 中部格式保留原草稿，只改变实际 Native 选区，避免将整格替换当格式功能。
                    let selection = if selected_case {
                        NSRange::new(1, 3)
                    } else {
                        NSRange::new(0, 1)
                    };
                    step(&current, || actual.setSelectedRange(selection))?;
                    let text = step(&current, || actual.string())?;
                    let expected = if selected_case {
                        original
                    } else {
                        ns_string!("甲")
                    };
                    if !step(&current, || text.isEqualToString(expected))?
                        || step(&current, || actual.selectedRange())? != selection
                    {
                        return Err("无变化格式没有准备真实“甲”子选区".into());
                    }
                } // 不把 shared editor 或 Native 字符串持到菜单队列/实际格式命令之后。
                let selection_before = {
                    let document = editor.document().borrow();
                    if document
                        .current_revision()
                        .map_err(|cause| cause.to_string())?
                        != facts.baseline
                        || document.is_dirty()
                        || document.can_undo()
                        || document.can_redo()
                    {
                        return Err("真实字段草稿提前改变 canonical 源或历史".into());
                    }
                    document.selection()
                };
                let controller = step(&current, || surface.controller.clone())?;
                let view_identity = std::ptr::from_ref(&**view).addr();
                Ok((controller, view_identity, field, selection_before))
            })?;
        if self.case == NativeMarkdownTableAppendCase::ToolbarFormatSelection {
            // 原字段准备已完成；释放局部Native claim后才点击，按钮必须自行保住刚才的子选区。
            let mut updated = facts;
            updated.selection = selection_before;
            self.facts.replace(Some(updated));
            drop(field);
            drop(controller);
            return self.click_toolbar_button(0, 103);
        }
        // surface 已归还 ACTIVE；此时才允许原菜单 capture，不能在借出的窗口中另造菜单票。
        let intent = NativeMarkdownWorkspaceEditIntent::capture(&controller)
            .ok_or("无变化格式没有原菜单票")?;
        intent.perform(103)?;
        drop(intent);
        if self.case == NativeMarkdownTableAppendCase::FormatSelection {
            return self.verify_selected_format(&facts, &field, view_identity, selection_before);
        }
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            let editor = surface.editor.as_ref().ok_or("无变化格式后编辑器缺失")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if !current()
                || std::ptr::from_ref(&**view).addr() != view_identity
                || view.ivars().active_table_cell.borrow().is_some()
                || !view.ivars().pending_table_cell_activation.is_empty()
                || view.table_append_pending()
            {
                return Err("无变化格式未正常结束原格，或错误启动了后继请求".into());
            }
            let value = step(&current, || field.stringValue())?;
            let units = step(&current, || value.length())?;
            drop(value); // 原字段值的临时 claim 先归还；其释放不能让后继正文冒充本次结果。
            if !current()
                || !step(&current, || field.isHidden())?
                || step(&current, || field.currentEditor())?.is_some()
                || units != 0
            {
                return Err("无变化格式未实际辞任、隐藏并清空原字段".into());
            }
            let document = editor.document().borrow();
            const EXPECTED: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | **甲** |\n";
            if document
                .current_revision()
                .map_err(|cause| cause.to_string())?
                != facts.baseline
                || document.byte_len() != EXPECTED.len()
                || document.utf16_len() != EXPECTED.encode_utf16().count()
                || document.selection() != selection_before
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
            {
                return Err("无变化格式改变实际源字节、正文选择或 canonical 历史".into());
            }
            for (offset, expected) in EXPECTED.encode_utf16().enumerate() {
                // 固定夹具逐 unit 核内容，不物化实际正文；revision 同时认证真实 UTF8 字节未变。
                if document
                    .utf16_code_unit_at(offset)
                    .map_err(|cause| cause.to_string())?
                    != expected
                {
                    return Err("无变化格式实际源与固定夹具不同".into());
                }
            }
            println!(
                "native_table_format_unchanged source_bytes=true generation_unchanged=true canonical_history_unchanged=true actual_field_hidden_clear_resigned=true formal=false physical_ime=false"
            );
            Ok(())
        })
    }

    /// 中部真实选区只提交一次；随后用原正文 Undo/Redo 和生产保存核字符、选择与文件字节。
    fn verify_selected_format(
        &self,
        facts: &AppendFacts,
        field: &NSTextField,
        view_identity: usize,
        selection_before: MarkdownTextSelection,
    ) -> Result<(), String> {
        const BEFORE: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 前中😀后 |\n";
        const AFTER: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 前**中😀**后 |\n";
        let cell_start = "| 编号 | 值 |\n| --- | --- |\n| 末项 | "
            .encode_utf16()
            .count();
        let expected = MarkdownTextSelection {
            anchor_utf16: cell_start + 3,
            head_utf16: cell_start + 6,
        };
        let first = facts.generation.checked_add(1).ok_or("格式代次溢出")?;
        let undone = first.checked_add(1).ok_or("格式 Undo 代次溢出")?;
        let redone = undone.checked_add(1).ok_or("格式 Redo 代次溢出")?;
        let expect_fixture =
            |document: &MarkdownDocumentBuffer, text: &str| -> Result<(), String> {
                if document.byte_len() != text.len()
                    || document.utf16_len() != text.encode_utf16().count()
                {
                    return Err("格内格式的固定源长度不同".into());
                }
                for (offset, expected) in text.encode_utf16().enumerate() {
                    // 仅核本固定小表，逐 unit 读取原 canonical；不产生实际正文 String 或全表投影。
                    if document
                        .utf16_code_unit_at(offset)
                        .map_err(|cause| cause.to_string())?
                        != expected
                    {
                        return Err("格内格式改动了目标子选区之外的实际字节".into());
                    }
                }
                Ok(())
            };
        let after = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, facts, first);
            let editor = surface.editor.as_ref().ok_or("中部格式后实际编辑器缺失")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if !current()
                || std::ptr::from_ref(&**view).addr() != view_identity
                || view.ivars().active_table_cell.borrow().is_some()
                || !view.ivars().pending_table_cell_activation.is_empty()
                || view.table_append_pending()
            {
                return Err("中部格式未只提交一次或未结束原格".into());
            }
            let value = step(&current, || field.stringValue())?;
            let units = step(&current, || value.length())?;
            drop(value);
            if !current()
                || units != 0
                || !step(&current, || field.isHidden())?
                || step(&current, || field.currentEditor())?.is_some()
                || step(&current, || view.native_ordered_selection_range())?
                    .map_err(|cause| cause.to_string())?
                    != NSRange::new(expected.anchor_utf16, 3)
            {
                return Err("中部格式未清字段或真实 Native after 不是“中😀”".into());
            }
            let document = editor.document().borrow();
            expect_fixture(&document, AFTER)?;
            if document.selection() != expected
                || !document.is_dirty()
                || !document.can_undo()
                || document.can_redo()
            {
                return Err("中部格式没有保留准确 after 和唯一原字符历史".into());
            }
            document
                .current_revision()
                .map_err(|cause| cause.to_string())
        })?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, facts, first);
            let editor = surface.editor.as_ref().ok_or("格式 Undo 前编辑器缺失")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(view.as_super());
            if !current() {
                return Err("格式 Undo 前原动作已撤销".into());
            }
            // SAFETY: 原实际正文接收既有 undo:；不走字段原生历史或新增物理快捷键。
            unsafe {
                input.doCommandBySelector(sel!(undo:));
            }
            if !self.current(surface, facts, undone) {
                return Err("一次格式 Undo 没有产生准确下一代".into());
            }
            let document = editor.document().borrow();
            expect_fixture(&document, BEFORE)?;
            if document
                .current_revision()
                .map_err(|cause| cause.to_string())?
                != facts.baseline
                || document.selection() != selection_before
                || document.is_dirty()
                || document.can_undo()
                || !document.can_redo()
            {
                return Err("一次 Undo 未恢复完整原源、选择和历史".into());
            }
            Ok(())
        })?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, facts, undone);
            let editor = surface.editor.as_ref().ok_or("格式 Redo 前编辑器缺失")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(view.as_super());
            if !current() {
                return Err("格式 Redo 前原动作已撤销".into());
            }
            // SAFETY: 同一实际正文消费唯一 redo:，只接受预声明下一代而不是返回后重新领权。
            unsafe {
                input.doCommandBySelector(sel!(redo:));
            }
            let current_after = || self.current(surface, facts, redone);
            if !current_after()
                || step(&current_after, || view.native_ordered_selection_range())?
                    .map_err(|cause| cause.to_string())?
                    != NSRange::new(expected.anchor_utf16, 3)
            {
                return Err("一次 Redo 未恢复准确 Native after".into());
            }
            let document = editor.document().borrow();
            expect_fixture(&document, AFTER)?;
            if document
                .current_revision()
                .map_err(|cause| cause.to_string())?
                != after
                || document.selection() != expected
                || !document.is_dirty()
                || !document.can_undo()
                || document.can_redo()
            {
                return Err("一次 Redo 未恢复格式字节、选择和历史".into());
            }
            Ok(())
        })?;
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, facts, redone) {
                return Err("格式保存前原窗口或正文已撤权".into());
            }
            surface.save_current_document()?; // 生产 session 保存流，不由测试直接写格式后的文件。
            if !self.current(surface, facts, redone) {
                return Err("格式保存改变了原 owner 或字符代次".into());
            }
            let document = surface
                .editor
                .as_ref()
                .ok_or("保存后编辑器缺失")?
                .document()
                .borrow();
            if document.is_dirty()
                || document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != after
                || document.selection() != expected
                || !document.can_undo()
                || document.can_redo()
            {
                return Err("生产保存改变了正文、选择或原 Undo".into());
            }
            Ok(())
        })?;
        let mut file = std::fs::File::open(&self.path).map_err(|cause| cause.to_string())?;
        let mut bytes = [0_u8; 256];
        std::io::Read::read_exact(&mut file, &mut bytes[..AFTER.len()])
            .map_err(|cause| cause.to_string())?;
        let mut tail = [0_u8; 1];
        if &bytes[..AFTER.len()] != AFTER.as_bytes()
            || std::io::Read::read(&mut file, &mut tail).map_err(|cause| cause.to_string())? != 0
        {
            return Err("生产保存的真实文件字节或 EOF 不准确".into());
        }
        drop(file); // 原有 Return/strict close 前归还测试读取 FD。
        let mut updated = facts.clone();
        updated.after = Some(after); // 仅记录实际已保存版本，供原 strict-close 后磁盘核对。
        self.facts.replace(Some(updated));
        println!(
            "native_table_format_selection actual_field_range=1..4 source_only_selected_text=true generation_delta=1 exact_native_after=true single_undo_redo=true production_save_exact_bytes=true physical_keyboard=false formal=false"
        );
        Ok(())
    }

    /// 两个输入组分别跨真实事件；本地Undo与工具栏历史必须持续属于同一活动字段。
    pub(crate) fn advance_toolbar_field_history(&self, stage: u8) -> Result<(), String> {
        let mut facts = self.facts()?;
        if stage != 0 {
            // 上一事件已执行真实按钮；现在认证实际卡片、原字段和历史，而非点击返回值。
            self.verify_information_card(&facts, stage != 4)?;
            if stage == 2 {
                self.click_information_control(&facts, true)?;
                self.verify_information_card(&facts, true)?; // 刷新词数也必须保持原格、草稿和选择。
            }
        }
        if stage == 0 {
            let activation = self
                .activation_receipt
                .borrow()
                .as_ref()
                .map(Rc::clone)
                .ok_or("字段历史缺少原激活收据")?;
            if activation.outcome() != Some(NativeMarkdownTableCellOutcome::Activated)
                || activation.original_generation() != facts.generation
            {
                return Err("字段历史未完成原单格激活".into());
            }
        }
        let selection = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            if !current() {
                return Err("本地字段历史期间canonical或窗口已改变".into());
            }
            let editor = surface.editor.as_ref().ok_or("字段历史没有原正文")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            let coordinate = view
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .map(|cell| cell.coordinate)
                .ok_or("工具栏历史提前结束了原活动格")?;
            if coordinate.row_index != 1 || coordinate.column_index != 1 {
                return Err("字段历史转移到了其他单元格".into());
            }
            let field =
                step(&current, || view.existing_table_cell_editor())?.ok_or("原字段已丢失")?;
            if step(&current, || field.isHidden())? {
                return Err("工具栏历史隐藏了原草稿字段".into());
            }
            let actual = step(&current, || field.currentEditor())?
                .ok_or("工具栏历史辞任了原field editor")?
                .downcast::<NSTextView>()
                .map_err(|_| "字段历史的实际editor类型不符")?;
            let old_editor = self.field_history_editor.borrow().clone(); // 仅复制Weak，不跨Native持RefCell借用。
            if stage == 0 {
                self.field_history_editor
                    .replace(Some(Rc::new(Weak::from_retained(&actual))));
            } else {
                let original = step(&current, || old_editor.as_ref().unwrap().load())?
                    .ok_or("原field editor已经释放")?;
                if !std::ptr::eq(&*original, &*actual) {
                    return Err("工具栏历史借用了后继field editor".into());
                }
            }
            let manager = step(&current, || {
                surface
                    .controller
                    .ivars()
                    .undo
                    .cached_manager(&surface.controller.ivars().owned)
            })?
            .ok_or("字段输入没有原已缓存的NSUndoManager")?;
            let actual_manager =
                step(&current, || actual.undoManager())?.ok_or("当前字段没有原撤销记录")?;
            if !std::ptr::eq(&*manager, &*actual_manager) {
                return Err("字段历史转移到了不同NSUndoManager".into());
            }
            drop(actual_manager);
            let (expected_text, expected_caret, can_redo) = match stage {
                0 => (ns_string!("前中😀后"), None, false), // 原格由真实activation呈现；第一输入会明确替换它。
                1 | 3 | 5 => (ns_string!("甲"), Some(1), matches!(stage, 3 | 5)),
                _ => (ns_string!("甲乙"), Some(2), false), // 第二组完成和每次本地Redo都必须恢复第二字。
            };
            let value = step(&current, || actual.string())?;
            let actual_selection = step(&current, || actual.selectedRange())?;
            if !step(&current, || value.isEqualToString(expected_text))?
                || expected_caret.is_some_and(|caret| actual_selection != NSRange::new(caret, 0))
                || step(&current, || manager.canUndo())? != (stage != 0)
                || step(&current, || manager.canRedo())? != can_redo
            {
                return Err(format!("字段真实草稿、选择或本地历史不符：stage={stage}"));
            }
            drop(value);
            let selection = {
                let document = editor.document().borrow();
                if document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != facts.baseline
                    || document.is_dirty()
                    || document.can_undo()
                    || document.can_redo()
                    || stage != 0 && document.selection() != facts.selection
                {
                    return Err("工具栏本地历史提前提交了canonical源或历史".into());
                }
                document.selection()
            };
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*actual);
            if stage == 1 {
                if !step(&current, || input.hasMarkedText())?
                    || step(&current, || input.markedRange())? != NSRange::new(0, 1)
                    || step(&current, || actual.selectedRange())? != NSRange::new(1, 0)
                {
                    return Err("打开信息卡确认、丢弃或移动了原真实marked输入".into());
                }
                println!(
                    "native_information_card marked_range_preserved=true native_field_selection_preserved=true original_field_editor_same=true canonical_history_unchanged=true synthetic_input_client=true physical_ime=false formal=false"
                );
                step(&current, || input.unmarkText())?; // 只有此明确合成输入动作结束组合，信息按钮本身无此权限。
                // 第一用户输入包含上一个事件启动的marked阶段；只有组合真正结束后才能
                // 分隔它与下一次“乙”输入。原来在setMarked之前break会被后继组合重新接续。
                step(&current, || actual.breakUndoCoalescing())?;
            }
            if step(&current, || input.hasMarkedText())? {
                return Err("合成字段历史不应已有IME组合".into());
            }
            match stage {
                0 | 1 => {
                    let (text, range) = if stage == 0 {
                        (ns_string!("甲"), NSRange::new(0, 5))
                    } else {
                        (ns_string!("乙"), NSRange::new(1, 0))
                    };
                    // SAFETY: 同一真实field editor接收明确范围内的有限输入；不直接写control值或伪造Undo记录。
                    step(&current, || unsafe {
                        input.insertText_replacementRange(text, range)
                    })?;
                    if stage == 0 {
                        // SAFETY: 同一真实NSTextInputClient把已有“甲”标为组合文本；字符与caret不变，
                        // 原窗口事件中的信息按钮必须完整保留此marked范围，下一阶段才明确unmark。
                        step(&current, || unsafe {
                            input.setMarkedText_selectedRange_replacementRange(
                                ns_string!("甲"),
                                NSRange::new(1, 0),
                                NSRange::new(0, 1),
                            )
                        })?;
                    } else {
                        // 第二个完整输入保持原break；第一输入的分隔已在本阶段unmark后完成。
                        step(&current, || actual.breakUndoCoalescing())?;
                    }
                }
                2 => {
                    let before = {
                        let document = editor.document().borrow();
                        (
                            document.generation(),
                            document.byte_len(),
                            document.selection(),
                        )
                    };
                    // SAFETY: 原实际NSTextInputClient执行标准字段undo:；不伪称物理键盘或直接改草稿。
                    let result = step(&current, || unsafe {
                        input.doCommandBySelector(sel!(undo:))
                    });
                    let after = {
                        let document = editor.document().borrow();
                        (
                            document.generation(),
                            document.byte_len(),
                            document.selection(),
                            document.can_undo(),
                            document.can_redo(),
                        )
                    }; // 只取原Doc标量，不能把此处的新状态当作旧Undo续权。
                    let active = view
                        .ivars()
                        .active_table_cell
                        .borrow()
                        .as_ref()
                        .map(|cell| cell.coordinate);
                    let associated = field
                        .currentEditor()
                        .is_some_and(|client| std::ptr::eq(&*client, actual.as_super()));
                    println!(
                        "native_toolbar_field_local_undo before={before:?} after={after:?} original_width={} actual_width={} active={active:?} same_field_editor={associated} field_hidden={} manager_can_undo={} manager_can_redo={} original_current={} formal=false",
                        facts.width,
                        editor
                            .backend
                            .text_storage
                            .ivars()
                            .table_projection_width_key
                            .get(),
                        field.isHidden(),
                        manager.canUndo(),
                        manager.canRedo(),
                        current()
                    );
                    result?; // 第一原权限错误原样返回；诊断不把已提交或已结束字段的状态当本地Undo成功。
                }
                6 => {
                    drop(manager);
                    drop(actual);
                    drop(field); // 正常commit会辞任原字段；所有本地强claim先结束，不替Native保活。
                    if !view.commit_active_table_cell()? {
                        return Err("真实字段草稿正常提交没有产生唯一源变更".into());
                    }
                    return Ok(selection); // canonical结果由下一事件按g+1/源/after/唯一历史独立认证。
                }
                _ => {} // 3/4/5只观察原toolbar回调结果；surface归还后才发下一个真实按钮。
            }
            Ok(selection)
        })?;
        if stage == 0 {
            facts.selection = selection;
            self.facts.replace(Some(facts));
        }
        match stage {
            0 => self.click_toolbar_button(0, 6), // 原真实输入仍marked；信息动作只能只读打开卡片。
            1 => {
                self.click_information_control(&self.facts()?, false)?;
                self.verify_information_card(&self.facts()?, false)?;
                self.click_toolbar_button(0, 6) // 原卡内关闭已同步完成，再由原信息按钮重开。
            }
            3 => {
                self.click_toolbar_button(0, 23)?;
                self.click_toolbar_button(0, 6) // 同一事件先请求原Redo，再收起原卡，下一阶段分开认证结果。
            }
            4 => {
                self.click_toolbar_button(0, 22)?;
                self.click_toolbar_button(0, 6) // 保留原Undo事件，重复打开必须复用首次原生根。
            }
            5 => self.click_toolbar_button(0, 23),
            _ => Ok(()), // 输入/本地Undo/commit都已发出一次，下一原事件只认证，不循环重试。
        }
    }

    /// 只观察原信息卡的具体已知控件，不遍历AppKit子树、构造影子控件或以对象数量判通过。
    fn verify_information_card(&self, facts: &AppendFacts, visible: bool) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, facts, facts.generation);
            if !current() {
                return Err("信息卡观察失去原文稿权限".into());
            }
            let panel = surface
                .controller
                .ivars()
                .information_panel
                .borrow()
                .clone()
                .ok_or("真实信息动作没有构造信息卡")?;
            let (root, scroll, values, close, refresh) = panel.probe_controls();
            let first = self.information_root.borrow().clone();
            if let Some(first) = first {
                let first =
                    step(&current, || first.load())?.ok_or("重复打开前原信息卡根已被替换")?;
                if !std::ptr::eq(&*first, root) {
                    return Err("反复开关信息卡重新分配原生控件".into());
                }
            } else {
                self.information_root
                    .replace(Some(step(&current, || Weak::from(root))?));
            }
            // SAFETY: 原实际卡片的父关系只在AppKit主线程读取；无状态借用跨Native。
            let parent = step(&current, || unsafe { root.superview() })?;
            if !visible {
                if parent.is_some() {
                    return Err("收起信息卡后仍挂接正文父视图".into());
                }
                for value in values {
                    let text = step(&current, || value.stringValue())?;
                    if step(&current, || text.length())? != 0
                        || step(&current, || value.toolTip())?.is_some()
                    {
                        return Err("隐藏卡片仍持有旧标题、路径、统计或提示".into());
                    }
                }
                return Ok(());
            }
            let content =
                step(&current, || surface.panel.contentView())?.ok_or("信息卡原窗口没有内容根")?;
            if parent
                .as_ref()
                .is_none_or(|parent| !std::ptr::eq(&**parent, &*content))
            {
                return Err("信息卡没有在原窗口正文内显示".into());
            }
            let editor = surface.editor.as_ref().ok_or("信息卡观察缺少原编辑器")?;
            let expected = {
                let document = editor.document().borrow();
                if document
                    .current_revision()
                    .map_err(|error| error.to_string())?
                    != facts.baseline
                    || document.is_dirty()
                    || document.can_undo()
                    || document.can_redo()
                {
                    return Err("只读信息卡提前提交了单格草稿或canonical历史".into());
                }
                [
                    "工作负载".to_owned(),
                    "工作负载.md".to_owned(),
                    document.character_count().to_string(),
                    document.word_count().to_string(),
                    document.line_count().to_string(),
                    document.byte_len().to_string(),
                ]
            };
            for (value, expected) in values.iter().zip(expected) {
                let text = step(&current, || value.stringValue())?;
                if !step(&current, || {
                    text.isEqualToString(&NSString::from_str(&expected))
                })? {
                    return Err("信息卡标题、相对路径或原canonical聚合不同".into());
                }
            }
            let modified = step(&current, || values[6].stringValue())?;
            if step(&current, || modified.length())? == 0
                || step(&current, || modified.isEqualToString(ns_string!("未获取")))?
            {
                return Err("合成文件的信息卡没有真实修改时间".into());
            }
            for (button, explanation) in [
                (close, ns_string!("关闭文档信息")),
                (refresh, ns_string!("刷新当前文档词数")),
            ] {
                if !step(&current, || button.accessibilityLabel())?
                    .is_some_and(|label| label.isEqualToString(explanation))
                {
                    return Err("信息卡关闭或刷新按钮缺少原中文无障碍说明".into());
                }
                if !step(&current, || button.refusesFirstResponder())?
                    || !step(&current, || button.isEnabled())?
                {
                    return Err("信息卡动作会抢文本焦点或不可用".into());
                }
            }
            let frame = step(&current, || root.frame())?;
            let body = step(&current, || editor.backend.scroll_view.frame())?;
            if frame.size.width != 270.0
                || frame.size.height <= 0.0
                || (frame.origin.x + frame.size.width - (body.origin.x + body.size.width - 14.0))
                    .abs()
                    > 0.1
                || (frame.origin.y + frame.size.height - (body.origin.y + body.size.height - 12.0))
                    .abs()
                    > 0.1
                || frame.origin.y < body.origin.y
            {
                return Err("信息卡不是原正文右上角浮层，或越过正文边界".into());
            }
            let clip = step(&current, || scroll.contentView())?;
            let document =
                step(&current, || scroll.documentView())?.ok_or("信息卡缺少实际可滚动内容")?;
            let content_height = step(&current, || document.frame())?.size.height;
            let viewport = step(&current, || clip.bounds())?;
            if viewport.size.height <= 0.0
                || content_height + 0.1 < viewport.size.height
                || (viewport.origin.y + viewport.size.height - content_height).abs() > 0.1
            {
                return Err("首次或重复打开信息卡未从内容顶端显示".into());
            }
            println!(
                "native_information_card visible=true original_root_reused=true original_doc_revision_unchanged=true title_path_counts=true original_overlay_frame=true formal=false physical_ime=false"
            );
            Ok(())
        })
    }

    /// 卡内关闭/刷新沿实际按钮target/action执行；不直接调用hide、刷新helper或改控件内容。
    fn click_information_control(
        &self,
        facts: &AppendFacts,
        refresh_words: bool,
    ) -> Result<(), String> {
        let button = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, facts, facts.generation);
            let panel = surface
                .controller
                .ivars()
                .information_panel
                .borrow()
                .clone()
                .ok_or("信息卡尚未真实打开")?;
            let (_, _, _, close, refresh) = panel.probe_controls();
            let button = if refresh_words { refresh } else { close };
            step(&current, || button.retain())
        })?;
        // SAFETY: 实际已装配的原按钮仅借用同一主线程controller，不伪造selector或Native结果。
        unsafe { button.performClick(None) };
        drop(button);
        Ok(())
    }

    /// 点击实际装配的按钮；原controller自行捕获意图并排队，调用返回不是编辑完成证明。
    fn click_toolbar_button(&self, delta: u64, tag: isize) -> Result<(), String> {
        let facts = self.facts()?;
        let generation = facts
            .generation
            .checked_add(delta)
            .ok_or("工具栏代次溢出")?;
        let button = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, generation);
            if !current() {
                return Err("工具栏点击前原文档已改变".into());
            }
            let controls = surface.controller.ivars().workspace_controls.get().unwrap();
            for control in controls.iter().skip(NATIVE_MARKDOWN_TOOLBAR_START) {
                let Some(button) = control.downcast_ref::<NSButton>() else {
                    continue; // 非按钮不代发动作；实际格式与历史仍按原自有按钮身份和tag发送。
                };
                if step(&current, || button.tag())? == tag {
                    if !step(&current, || button.isEnabled())?
                        || !step(&current, || button.refusesFirstResponder())?
                    {
                        return Err("工具栏按钮不可用或会夺取文本焦点".into());
                    }
                    return step(&current, || button.retain());
                }
            }
            Err("实际工具栏没有所需按钮".into())
        })?;
        // ACTIVE已归还，正常target callback才能捕原Cell子选区；不直接调用菜单实现或历史方法。
        // SAFETY: 主线程上实际已安装且启用的NSButton，action/target均由产品装配并仍属于原owner。
        unsafe { button.performClick(None) };
        drop(button); // 不让诊断持有按钮跨后续事件或真实关闭，结果由下一阶段的canonical事实核验。
        Ok(())
    }

    /// 每次只在上一真实按钮回调之后认证一个结果，再发起下一个按钮；不重放原点击。
    pub(crate) fn verify_toolbar_format_history(&self, stage: u8) -> Result<(), String> {
        const BEFORE: &str = "| 编号 | 值 |\n| --- | --- |\n| 末项 | 前中😀后 |\n";
        let field_history = self.case == NativeMarkdownTableAppendCase::ToolbarFieldHistory;
        let after = if field_history {
            "| 编号 | 值 |\n| --- | --- |\n| 末项 | 甲乙 |\n" // 两个本地组正常提交成一次源替换，不能残留字段历史到canonical。
        } else {
            "| 编号 | 值 |\n| --- | --- |\n| 末项 | 前**中😀**后 |\n"
        };
        let mut facts = self.facts()?;
        let delta = u64::from(stage) + 1;
        let generation = facts
            .generation
            .checked_add(delta)
            .ok_or("工具栏历史代次溢出")?;
        let formatted = stage != 1;
        let cell_start = "| 编号 | 值 |\n| --- | --- |\n| 末项 | "
            .encode_utf16()
            .count();
        let selected = if field_history {
            MarkdownTextSelection::caret(cell_start + 2) // 普通提交沿原格替换末尾caret，不借用字段原生历史。
        } else {
            MarkdownTextSelection {
                anchor_utf16: cell_start + 3,
                head_utf16: cell_start + 6,
            }
        };
        let expected_selection = if formatted { selected } else { facts.selection };
        let expected_text = if formatted { after } else { BEFORE };
        let revision = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, generation);
            let editor = surface.editor.as_ref().ok_or("工具栏完成后没有原编辑器")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if !current()
                || view.ivars().active_table_cell.borrow().is_some()
                || !view.ivars().pending_table_cell_activation.is_empty()
                || view.table_append_pending()
            {
                return Err("实际工具栏动作没有完成唯一提交或结束原字段".into());
            }
            let field = step(&current, || view.existing_table_cell_editor())?
                .ok_or("工具栏原复用字段丢失")?;
            let value = step(&current, || field.stringValue())?;
            if step(&current, || value.length())? != 0
                || !step(&current, || field.isHidden())?
                || step(&current, || field.currentEditor())?.is_some()
            {
                return Err("工具栏格式后原字段没有实际清空、隐藏并辞任".into());
            }
            drop(value);
            drop(field);
            let native = step(&current, || view.native_ordered_selection_range())?
                .map_err(|cause| cause.to_string())?;
            let start = expected_selection
                .anchor_utf16
                .min(expected_selection.head_utf16);
            let end = expected_selection
                .anchor_utf16
                .max(expected_selection.head_utf16);
            if native != NSRange::new(start, end - start) {
                return Err("工具栏动作未恢复准确真实Native选区".into());
            }
            let document = editor.document().borrow();
            if document.byte_len() != expected_text.len()
                || document.utf16_len() != expected_text.encode_utf16().count()
                || document.selection() != expected_selection
                || document.is_dirty() != formatted
                || document.can_undo() != formatted
                || document.can_redo() == formatted
            {
                return Err("工具栏字符、选择或单次Undo/Redo状态不符".into());
            }
            for (offset, expected) in expected_text.encode_utf16().enumerate() {
                // 固定小表逐unit核原canonical；邻格和选区外汉字必须原样，不物化实际正文。
                if document
                    .utf16_code_unit_at(offset)
                    .map_err(|cause| cause.to_string())?
                    != expected
                {
                    return Err("真实工具栏改动了固定子选区以外的源".into());
                }
            }
            let revision = document
                .current_revision()
                .map_err(|cause| cause.to_string())?;
            if stage == 1 && revision != facts.baseline
                || stage == 2 && Some(revision) != facts.after
            {
                return Err("工具栏一次Undo/Redo未恢复原完整revision".into());
            }
            Ok(revision)
        })?;
        if stage == 0 {
            facts.after = Some(revision);
            facts.after_selection = Some(selected);
            self.facts.replace(Some(facts));
            return self.click_toolbar_button(delta, 22);
        }
        if stage == 1 {
            return self.click_toolbar_button(delta, 23);
        }
        // 三个已完成事件都通过后才用生产保存；保存不能新增字符历史或改变选区。
        with_active_native_markdown_surface(|surface| {
            if !self.current(surface, &facts, generation) {
                return Err("工具栏保存前原文档已改变".into());
            }
            surface.save_current_document()?;
            if !self.current(surface, &facts, generation) {
                return Err("工具栏生产保存改变了原字符代次".into());
            }
            let document = surface.editor.as_ref().unwrap().document().borrow();
            if document.is_dirty()
                || document.selection() != selected
                || !document.can_undo()
                || document.can_redo()
                || document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != revision
            {
                return Err("工具栏生产保存改变了字符、选择或原历史".into());
            }
            Ok(())
        })?;
        let mut file = std::fs::File::open(&self.path).map_err(|cause| cause.to_string())?;
        let mut bytes = [0_u8; 256];
        std::io::Read::read_exact(&mut file, &mut bytes[..after.len()])
            .map_err(|cause| cause.to_string())?;
        let mut tail = [0_u8; 1];
        if &bytes[..after.len()] != after.as_bytes()
            || std::io::Read::read(&mut file, &mut tail).map_err(|cause| cause.to_string())? != 0
        {
            return Err("真实工具栏生产保存的字节或EOF不准确".into());
        }
        drop(file); // 正常Return/strict-close前归还本测试FD，不替产品保留源句柄。
        println!(
            "native_toolbar_canonical_history field_history={field_history} actual_format_button={} actual_undo_22=true actual_redo_23=true exact_native_after=true single_undo_redo=true production_save_exact_bytes=true formal=false physical_keyboard=false",
            !field_history,
        );
        Ok(())
    }

    /// 字段激活前由真实窗口resize通知完成布局，往返后正文、选择和历史必须仍是同一基线。
    fn verify_toolbar_resize(&self, facts: &AppendFacts) -> Result<(), String> {
        let panel = with_active_native_markdown_surface(|surface| {
            if !self.current(surface, facts, facts.generation) {
                return Err("缩放前原窗口已改变".into());
            }
            Ok(surface.panel.clone())
        })?;
        let original = panel
            .contentView()
            .ok_or("缩放窗口缺少内容根")?
            .frame()
            .size;
        let result: Result<(), String> = (|| {
            for size in [
                NSSize::new(900.0, 600.0),
                NSSize::new(NATIVE_MARKDOWN_WIDTH, NATIVE_MARKDOWN_HEIGHT),
                NSSize::new(1400.0, 900.0),
            ] {
                // ACTIVE已归还；setContentSize触发原windowDidResize，不在probe调用布局实现补签。
                panel.setContentSize(size);
                panel.displayIfNeeded();
                self.verify_actual_toolbar_frames(facts, size)?;
            }
            Ok(())
        })();
        panel.setContentSize(original); // 即便某一尺寸失败，也先恢复原大小，保留最初的验收错误。
        panel.displayIfNeeded();
        let restored = self.verify_actual_toolbar_frames(facts, original);
        drop(panel);
        result?;
        restored?;
        println!(
            "native_toolbar_resize actual_delegate=true minimum=900x600 larger=1400x900 original_size_restored=true source_selection_history_unchanged=true formal=false"
        );
        Ok(())
    }

    /// 仅此合成工具栏用例读取实际控件矩形、标题和中文AX；不调用生产几何函数推导“实际结果”。
    fn verify_actual_toolbar_frames(
        &self,
        facts: &AppendFacts,
        size: NSSize,
    ) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("缩放后没有原编辑器")?;
            if std::ptr::from_ref(&*surface.controller).addr() != facts.owner
                || !native_markdown_controller_has_foreground_authority(&surface.controller)
                || !WeakMarkdownDocument::ptr_eq(&Rc::downgrade(editor.document()), &facts.document)
            {
                return Err("缩放期间已替换原窗口或正文".into());
            }
            {
                let document = editor.document().borrow();
                if document.generation() != facts.generation
                    || document
                        .current_revision()
                        .map_err(|cause| cause.to_string())?
                        != facts.baseline
                    || document.selection() != facts.selection
                    || document.is_dirty()
                    || document.can_undo()
                    || document.can_redo()
                {
                    return Err("窗口缩放改变了源、选择或历史".into());
                }
            } // 所有canonical借用先结束，再读取实际AppKit控件。
            let content = surface.panel.contentView().ok_or("缩放后缺少实际内容根")?;
            if content.frame().size != size {
                return Err("窗口没有采用请求的真实内容尺寸".into());
            }
            let controls = surface.controller.ivars().workspace_controls.get().unwrap();
            let mode = surface
                .controller
                .ivars()
                .presentation_mode_button
                .get()
                .unwrap();
            let status = surface.controller.ivars().status_label.get().unwrap();
            if !surface
                .note_title
                .stringValue()
                .isEqualToString(ns_string!("工作负载"))
                || !surface
                    .note_title
                    .accessibilityLabel()
                    .is_some_and(|label| label.isEqualToString(ns_string!("当前笔记")))
            {
                return Err("实际标题带入扩展名或丢失中文AX说明".into());
            }
            if !mode.title().isEqualToString(ns_string!("源码"))
                || !mode.refusesFirstResponder()
                || !mode
                    .accessibilityLabel()
                    .is_some_and(|label| label.isEqualToString(ns_string!("切换源码显示")))
                || mode.state()
                    != if surface.presentation_mode == MarkdownPresentationMode::Source {
                        objc2_app_kit::NSControlStateValueOn
                    } else {
                        objc2_app_kit::NSControlStateValueOff
                    }
            {
                return Err("顶部原源码按钮的标签、焦点或真实模式状态不同".into());
            }
            let mut headers = vec![
                surface.note_title.frame(),
                surface.note_path.frame(),
                mode.frame(),
                controls[1].frame(),
            ];
            for (control, label) in controls
                .iter()
                .skip(NATIVE_MARKDOWN_TOOLBAR_START)
                .take(5)
                .zip([
                    "撤销当前笔记的上一步编辑",
                    "重做当前笔记的编辑",
                    "在当前笔记中查找",
                    "查看当前笔记信息",
                    "将当前笔记的原始Markdown全文复制到系统剪贴板",
                ])
            {
                if control.isHidden()
                    || !control.refusesFirstResponder()
                    || !control
                        .accessibilityLabel()
                        .is_some_and(|value| value.isEqualToString(&NSString::from_str(label)))
                {
                    return Err("实际常用按钮被隐藏、会夺焦或中文AX不同".into());
                }
                headers.push(control.frame());
            }
            for control in
                &controls[NATIVE_MARKDOWN_TOOLBAR_START..NATIVE_MARKDOWN_TOOLBAR_START + 2]
            {
                if control.isEnabled() {
                    return Err("空历史的撤销或重做按钮仍显示可用".into());
                } // 原Doc已确认无Undo/Redo，不因文档存在就启用历史。
            }
            // 这里按原React动作顺序读取真实装配按钮，不把生产布局结果当作oracle。
            let expected = [
                (3, "导入附件并在原光标处插入链接"),
                (100, "一级标题"),
                (101, "二级标题"),
                (102, "三级标题"),
                (103, "加粗"),
                (104, "斜体"),
                (105, "下划线"),
                (106, "删除线"),
                (107, "行内代码"),
                (108, "链接"),
                (110, "无序列表"),
                (111, "有序列表"),
                (112, "任务列表"),
                (109, "引用"),
                (118, "表格"),
                (113, "行内公式"),
                (117, "块级公式"),
                (114, "上标"),
                (116, "代码块"),
                (115, "分隔线"),
            ];
            let formats = &controls[NATIVE_MARKDOWN_TOOLBAR_START + 5..];
            if formats.len() != expected.len() {
                return Err("原附件或十九项一击格式入口有缺失或重复".into());
            }
            let strip = formats[0]
                .enclosingScrollView()
                .ok_or("格式按钮缺少真实水平滚动视口")?;
            let document_view = strip.documentView().ok_or("格式视口缺少真实按钮内容根")?;
            let mut previous_end = 0.0;
            for (control, (tag, label)) in formats.iter().zip(expected) {
                let button = control
                    .downcast_ref::<NSButton>()
                    .ok_or("原一击格式入口变成菜单或其它控件")?;
                let frame = button.frame();
                let same_scroll = button
                    .enclosingScrollView()
                    .is_some_and(|candidate| std::ptr::eq(&*candidate, &*strip));
                if !same_scroll
                    || button.tag() != tag
                    || button.isHidden()
                    || !button.isEnabled()
                    || !button.refusesFirstResponder()
                    || !button
                        .accessibilityLabel()
                        .is_some_and(|value| value.isEqualToString(&NSString::from_str(label)))
                    || frame.origin.x < previous_end
                    || frame.origin.y < 0.0
                    || frame.size.height != 29.0
                    || frame.origin.y + frame.size.height > document_view.frame().size.height
                    || frame.origin.x + frame.size.width > document_view.frame().size.width
                {
                    return Err("原十九格式顺序、局部按钮几何、焦点或中文AX不同".into());
                }
                previous_end = frame.origin.x + frame.size.width;
            }
            if strip.hasVerticalScroller()
                || !strip.hasHorizontalScroller()
                || strip.frame().size.height != 42.0
            {
                return Err("原格式栏没有保持42pt单行水平滚动".into());
            }
            let clip = strip.contentView();
            let original_origin = clip.bounds().origin;
            let first = formats[0].frame();
            let last = formats.last().unwrap().frame();
            let reachable: Result<(), String> = (|| {
                for frame in [last, first] {
                    // 在同一真实NSClipView滚至末/首入口，确认窄窗没有以隐藏动作代替横滚。
                    clip.scrollToPoint(NSPoint::new(
                        (frame.origin.x + frame.size.width - clip.bounds().size.width).max(0.0),
                        0.0,
                    ));
                    strip.reflectScrolledClipView(&clip);
                    let visible = clip.bounds();
                    if frame.origin.x < visible.origin.x
                        || frame.origin.x + frame.size.width > visible.origin.x + visible.size.width
                        || frame.origin.y < visible.origin.y
                        || frame.origin.y + frame.size.height
                            > visible.origin.y + visible.size.height
                    {
                        return Err("格式栏首项或末项不能沿真实水平视口完整到达".into());
                    }
                }
                Ok(())
            })();
            clip.scrollToPoint(original_origin);
            strip.reflectScrolledClipView(&clip); // 任何断言失败也先恢复原横滚位置，不影响后续真实格式点击。
            reachable?;
            headers.push(strip.frame()); // 按钮frame属于局部内容；全局不重叠核验只使用真实scroll根。
            let body = editor.backend.scroll_view().frame();
            let footers = surface
                .controller
                .ivars()
                .footer_labels
                .get()
                .unwrap()
                .each_ref()
                .map(|label| label.frame());
            let feedback = (!status.isHidden()).then(|| status.frame());
            let find_bar = editor.find_bar.borrow().clone();
            let find_frame = find_bar
                .as_ref()
                .and_then(|bar| bar.visible_frame_for_probe());
            if let (Some(feedback), Some(find)) = (feedback, find_frame)
                && NSIntersectsRect(feedback, find)
            {
                return Err("真实反馈行与查找栏互相覆盖".into());
            }
            for label in surface.controller.ivars().footer_labels.get().unwrap() {
                if label.isHidden() || label.font().is_none_or(|font| font.pointSize() != 11.0) {
                    return Err("原footer三列未按正常字号显示".into());
                }
            }
            for pair in footers.windows(2) {
                if NSIntersectsRect(pair[0], pair[1]) {
                    return Err("原footer三列相互重叠".into());
                }
            }
            for (index, frame) in headers.iter().enumerate() {
                if frame.origin.x < NATIVE_MARKDOWN_SIDEBAR_WIDTH
                    || frame.origin.y < size.height - sidebar_geometry::HEADER_HEIGHT
                    || frame.origin.x + frame.size.width > size.width
                    || frame.origin.y + frame.size.height > size.height
                    || frame.size.width <= 0.0
                    || frame.size.height <= 0.0
                    || NSIntersectsRect(*frame, body)
                    || footers
                        .iter()
                        .any(|footer| NSIntersectsRect(*frame, *footer))
                    || feedback.is_some_and(|feedback| NSIntersectsRect(*frame, feedback))
                {
                    return Err("真实工具栏超出窗口或遮住正文和底栏".into());
                }
                for other in &headers[index + 1..] {
                    if NSIntersectsRect(*frame, *other) {
                        return Err("真实工具栏按钮或标题发生重叠".into());
                    }
                }
            }
            let search = controls[0].frame();
            let list = surface
                .sidebar_list
                .table
                .enclosingScrollView()
                .ok_or("实际笔记列表缺少滚动视图")?
                .frame();
            for frame in [
                search,
                list,
                controls[2].frame(),
                controls[3].frame(),
                footers[0],
                footers[1],
                footers[2],
                body,
            ]
            .into_iter()
            .chain(feedback)
            {
                if frame.origin.x < 0.0
                    || frame.origin.y < 0.0
                    || frame.origin.x + frame.size.width > size.width
                    || frame.origin.y + frame.size.height > size.height
                {
                    return Err("缩放后搜索、列表、正文或底栏超出可见窗口".into());
                }
            }
            if NSIntersectsRect(search, list)
                || NSIntersectsRect(list, controls[2].frame())
                || NSIntersectsRect(list, controls[3].frame())
                || footers.iter().any(|footer| NSIntersectsRect(body, *footer))
                || feedback.is_some_and(|feedback| NSIntersectsRect(body, feedback))
            {
                return Err("真实搜索、列表或底栏相互遮挡".into());
            }
            Ok(())
        })
    }

    /// 两个负例只接受各自原请求的明确未提交终点，不能用slot消失或后来UI状态替代。
    pub(crate) fn verify_negative(&self) -> Result<(), String> {
        let expected = match self.case {
            NativeMarkdownTableAppendCase::Budget => {
                NativeMarkdownTableAppendOutcome::Failed { committed: false }
            }
            NativeMarkdownTableAppendCase::Cancel => {
                NativeMarkdownTableAppendOutcome::Cancelled { committed: false }
            }
            NativeMarkdownTableAppendCase::Positive
            | NativeMarkdownTableAppendCase::FormatUnchanged
            | NativeMarkdownTableAppendCase::CommitResigned
            | NativeMarkdownTableAppendCase::FormatSelection
            | NativeMarkdownTableAppendCase::ToolbarFormatSelection
            | NativeMarkdownTableAppendCase::ToolbarFieldHistory
            | NativeMarkdownTableAppendCase::PointerSelection
            | NativeMarkdownTableAppendCase::FindReplacementComposition => {
                return Err("正例不能进入负例断言".into());
            }
        };
        let receipt = self
            .receipt
            .borrow()
            .as_ref()
            .map(Rc::clone)
            .ok_or("负例缺少原追加收据")?;
        if receipt.outcome() != Some(expected) {
            return Err(format!("追加负例原终点不符：{:?}", receipt.outcome()));
        }
        let facts = self.facts()?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, facts.generation);
            if !current() {
                return Err("负例已改变原代次或窗口权限".into());
            }
            let editor = surface.editor.as_ref().ok_or("负例缺少实际编辑器")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            {
                let document = editor.document().borrow();
                if document
                    .current_revision()
                    .map_err(|error| format!("无法读取原文档revision：{error}"))?
                    != facts.baseline
                    || document.selection() != facts.selection
                {
                    return Err("负例改变了完整原root或选区".into());
                }
            } // canonical借用不跨接下来的Native字段读取。
            if view.table_append_pending() || !view.ivars().pending_table_cell_activation.is_empty()
            {
                return Err("负例终点之后仍有追加或重开请求".into());
            }
            match self.case {
                NativeMarkdownTableAppendCase::Budget => {
                    let active = view
                        .ivars()
                        .active_table_cell
                        .borrow()
                        .as_ref()
                        .map(|state| state.coordinate);
                    if active != Some(receipt.original_coordinate()) {
                        return Err("预算拒绝没有保留原末格".into());
                    }
                    (|| -> Result<(), String> {
                        let field = step(&current, || view.existing_table_cell_editor())?
                            .ok_or("预算拒绝后实际字段缺失")?;
                        let native = step(&current, || field.currentEditor())?
                            .ok_or("预算拒绝后field editor已丢失")?;
                        let field_editor = native
                            .downcast::<NSTextView>()
                            .map_err(|_| "预算反例field editor类型不符")?;
                        // 读取实际field editor的NSString；不走control.stringValue或一次复制整个草稿。
                        let value = step(&current, || field_editor.string())?;
                        if step(&current, || value.length())? != OVER_BUDGET_UNITS {
                            return Err("预算拒绝没有保留完整65537unit草稿".into());
                        }
                        let mut buffer = [0u16; 256];
                        let mut offset = 0;
                        while offset < OVER_BUDGET_UNITS {
                            let count = buffer.len().min(OVER_BUDGET_UNITS - offset);
                            // SAFETY: 已核实际长度；每次范围位于该值内，输出缓冲至少有count个u16。
                            step(&current, || unsafe {
                                value.getCharacters_range(
                                    NonNull::new(buffer.as_mut_ptr()).unwrap(),
                                    NSRange::new(offset, count),
                                );
                            })?;
                            if buffer[..count].iter().any(|unit| *unit != u16::from(b'A')) {
                                return Err("预算拒绝后的实际草稿内容不完整".into());
                            }
                            offset += count; // 固定256unit前进，不随源库大小缓存读取结果。
                        }
                        Ok(())
                    })()?;
                    if !current() {
                        return Err("预算草稿读取与临时claim析构后已撤权".into());
                    }
                    // 全部保留断言已完成，才经生产取消丢弃本测试超量草稿，允许真实Return保存原root。
                    step(&current, || view.cancel_active_table_cell())?;
                }
                NativeMarkdownTableAppendCase::Cancel => {
                    // 同栈已调用生产取消；这里仅认证原Cancelled(false)，绝不再触发一次取消充作成功。
                }
                NativeMarkdownTableAppendCase::Positive
                | NativeMarkdownTableAppendCase::FormatUnchanged
                | NativeMarkdownTableAppendCase::CommitResigned
                | NativeMarkdownTableAppendCase::FormatSelection
                | NativeMarkdownTableAppendCase::ToolbarFormatSelection
                | NativeMarkdownTableAppendCase::ToolbarFieldHistory
                | NativeMarkdownTableAppendCase::PointerSelection
                | NativeMarkdownTableAppendCase::FindReplacementComposition => {
                    return Err("正例不能消费负例原票".into());
                }
            }
            if view.ivars().active_table_cell.borrow().is_some()
                || !view.ivars().pending_table_cell_activation.is_empty()
                || view.table_append_pending()
            {
                return Err("负例清理后仍有活动格或待重开状态".into());
            }
            (|| -> Result<(), String> {
                if let Some(field) = step(&current, || view.existing_table_cell_editor())?
                    && (!step(&current, || field.isHidden())?
                        || step(&current, || field.currentEditor())?.is_some())
                {
                    return Err("负例生产取消未结束实际field editor".into());
                }
                Ok(())
            })()?;
            if !current() {
                return Err("负例清理的Native临时claim析构后撤权".into());
            }
            let document = editor.document().borrow();
            if document
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?
                != facts.baseline
                || document.selection() != facts.selection
            {
                return Err("负例清理改变了原root或完整选区".into());
            }
            let boundary = if self.case == NativeMarkdownTableAppendCase::Budget {
                "native_field_utf16" // 这是Native字段准入反例，不能冒称Core source budget已覆盖。
            } else {
                "input_cancel"
            };
            println!(
                "native_table_append_negative case={:?} passed=true committed=false boundary={boundary}",
                self.case
            );
            Ok(())
        })
    }

    pub(crate) fn verify_append_and_undo(&self) -> Result<(), String> {
        let receipt = self
            .receipt
            .borrow()
            .as_ref()
            .map(Rc::clone)
            .ok_or("追加缺少原收据")?;
        if receipt.outcome() != Some(NativeMarkdownTableAppendOutcome::Activated) {
            return Err(format!("追加原请求未完成真实激活：{:?}", receipt.outcome()));
        }
        let mut facts = self.facts()?;
        let generation = facts.generation.checked_add(1).ok_or("合成代次溢出")?;
        let (revision, selection) = with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, generation);
            if !current() {
                return Err("追加不是一次真实canonical提交".into());
            }
            let editor = surface.editor.as_ref().ok_or("追加完成后缺少编辑器")?;
            let view = &editor.backend.text_view;
            let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            let active = view
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .map(|state| (state.coordinate, state.source_utf16_range.clone()));
            if view.table_append_pending()
                || !view.ivars().pending_table_cell_activation.is_empty()
                || !active
                    .as_ref()
                    .is_some_and(|(cell, _)| cell.row_index == 2 && cell.column_index == 0)
            {
                return Err("追加收据与真实新行首格不一致".into());
            }
            (|| -> Result<(), String> {
                let field = step(&current, || view.existing_table_cell_editor())?
                    .ok_or("新行没有真实字段")?;
                if step(&current, || field.isHidden())? {
                    return Err("新行字段仍隐藏".into());
                }
                let frame = step(&current, || field.frame())?;
                let visible = step(&current, || view.visibleRect())?;
                if !NSIntersectsRect(frame, visible) {
                    return Err("新行字段不在实际clip内".into());
                }
                // selectText的void返回不能证明接管；必须取得实际NSTextView并核新空格的有限值。
                let native = step(&current, || field.currentEditor())?
                    .ok_or("新行字段没有接管实际field editor")?;
                let field_editor = native
                    .downcast::<NSTextView>()
                    .map_err(|_| "新行field editor类型不符")?;
                let value = step(&current, || field_editor.string())?;
                let units = step(&current, || value.length())?;
                if units > 256 {
                    return Err("新行field editor超过固定合成预算".into());
                }
                if units != 0 {
                    return Err("新行field editor仍含旧草稿或其他非空值".into());
                }
                Ok(())
            })()?; // 结束全部Native字段/文本临时claim，再核原提交票，不延长它们到正文Undo。
            if !current() {
                return Err("新行字段读取析构后撤权".into());
            }
            let authority = view
                .ivars()
                .active_table_cell
                .borrow()
                .as_ref()
                .and_then(|state| state.authority.as_ref().map(Rc::clone))
                .ok_or("追加后的活动格缺少同Ready源权限")?;
            {
                // 仅消费实际Reopen持有的有限Ready；不能恢复全表快照，或把Vec长度解释成全表行数。
                let document = editor.document().borrow();
                let table = authority
                    .window
                    .presentation_for(&document)
                    .map_err(|_| "追加后的Ready源已改变")?;
                if authority.header_byte_start != 0
                    || table.header_byte_range.start != 0
                    || table.column_count != 2
                    || table
                        .row(2)
                        .is_none_or(|row| row.row_ordinal != 2 || row.is_header)
                {
                    return Err("追加后的Ready不是原表绝对第2行两列".into());
                }
                if let Some(end) = authority
                    .window
                    .table_end_for(&document)
                    .map_err(|_| "追加后的真实End源已改变")?
                    && (end.header_byte_start() != 0 || end.last_body_row_ordinal() != Some(2))
                {
                    return Err("追加后已签发的真实End与新行不符".into());
                }
                // Ready可没有End：只有原append Rc证明旧真实End与一次事务，不从None猜全表尾。
                let source_range = |column| -> Result<std::ops::Range<usize>, String> {
                    let cell = table.cell(2, column).ok_or("新Ready缺少绝对row2逻辑列")?;
                    Ok(match &cell.source {
                        MarkdownTableWindowCellSource::Present(range) => range.clone(),
                        MarkdownTableWindowCellSource::ImplicitEmpty {
                            insertion_anchor_utf16,
                        } => *insertion_anchor_utf16..*insertion_anchor_utf16,
                    })
                };
                let first = source_range(0)?;
                if active.as_ref().map(|(_, range)| range) != Some(&first) {
                    return Err("实际新行字段没有绑定同Ready真实首格范围".into());
                }
                for column in 0..2 {
                    let range = source_range(column)?;
                    if range.len() > 256 {
                        return Err("新空格源范围超过固定合成预算".into());
                    }
                    for offset in range {
                        // Present逐unit认证为空白；ImplicitEmpty只有已验证锚点，不凭长度造补格字节。
                        if !matches!(document.utf16_code_unit_at(offset), Ok(0x20 | 0x09)) {
                            return Err("新行的真实cell源不是空格".into());
                        }
                    }
                }
                // 已知自有小夹具用固定expected逐unit核完整结果，既证明草稿也证明只新增一行；不复制实际正文。
                const EXPECTED_AFTER: &str =
                    "| 编号 | 值 |\n| --- | --- |\n| 末项 | 末格追加验收 |\n| | |\n";
                let expected_length = EXPECTED_AFTER.encode_utf16().count();
                if expected_length > 128 || document.utf16_len() != expected_length {
                    return Err("追加后的合成源长度不等于唯一一次预期提交".into());
                }
                for (offset, expected) in EXPECTED_AFTER.encode_utf16().enumerate() {
                    if document.utf16_code_unit_at(offset).ok() != Some(expected) {
                        return Err("末格草稿与新空行没有同次进入精确合成源".into());
                    }
                }
            } // 结束纯Doc借用后才进入生产field取消与正文Undo。
            let (revision, selection) = {
                let document = editor.document().borrow();
                (
                    document
                        .current_revision()
                        .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                    document.selection(),
                )
            };
            if self.case == NativeMarkdownTableAppendCase::CommitResigned {
                // 模拟真实 Return 已先结束共享 editor 的合法边界；不替换字段，不伪造 abort 返回值。
                let field = step(&current, || view.existing_table_cell_editor())?
                    .ok_or("已辞任提交回归缺少原空字段")?;
                step(&current, || field.abortEditing())?;
                if step(&current, || field.currentEditor())?.is_some() {
                    return Err("原空字段未实际辞任，不能冒充已无 editor 的普通提交".into());
                }
                step(&current, || view.commit_active_table_cell())??;
                let value = step(&current, || field.stringValue())?;
                let empty = step(&current, || value.length())? == 0;
                drop(value); // 原字段值先释放，再核后继原权限与隐藏事实。
                if !current()
                    || !empty
                    || !step(&current, || field.isHidden())?
                    || step(&current, || field.currentEditor())?.is_some()
                    || view.ivars().active_table_cell.borrow().is_some()
                {
                    return Err("合法已辞任字段未被普通提交正常清理".into());
                }
                {
                    let document = editor.document().borrow();
                    if document
                        .current_revision()
                        .map_err(|cause| cause.to_string())?
                        != revision
                        || document.selection() != selection
                        || !document.can_undo()
                        || document.can_redo()
                    {
                        return Err("已辞任空格提交改变原字节、选择或追加历史".into());
                    }
                }
                println!(
                    "native_table_commit_resigned actual_editor_none_before_commit=true source_and_selection_unchanged=true original_undo_preserved=true actual_field_cleared=true formal=false"
                );
            } else {
                // 原 positive 继续验证真实空字段取消，不能为新回归删掉已有交互覆盖。
                step(&current, || view.cancel_active_table_cell())?;
            }
            let input =
                ProtocolObject::<dyn NSTextInputClient>::from_ref(editor.backend.text_view());
            if !current() {
                return Err("Undo调用前原追加已失效".into());
            }
            // SAFETY: 原ACTIVE正文实现公开undo:；仅程序selector，不声称用户CmdZ已验证。
            unsafe {
                input.doCommandBySelector(sel!(undo:));
            }
            let undone = generation.checked_add(1).ok_or("Undo代次溢出")?;
            if !self.current(surface, &facts, undone) {
                return Err("一次正文Undo未产生固定下一代".into());
            }
            Ok((revision, selection))
        })?;
        facts.after = Some(revision);
        facts.after_selection = Some(selection);
        self.facts.replace(Some(facts));
        Ok(())
    }

    /// History动作预先声明唯一下一代；检查不能在Undo/Redo返回后重领新generation。
    pub(crate) fn verify_history(&self, redo: bool, final_check: bool) -> Result<(), String> {
        let facts = self.facts()?;
        let delta = if final_check {
            4
        } else if redo {
            2
        } else {
            3
        };
        let expected_generation = facts.generation.checked_add(delta).ok_or("历史代次溢出")?;
        with_active_native_markdown_surface(|surface| {
            let current = || self.current(surface, &facts, expected_generation);
            if !current() {
                return Err("追加历史代次不是预声明的一次变更".into());
            }
            let editor = surface.editor.as_ref().ok_or("历史验证缺少编辑器")?;
            let (revision, selection) = {
                let document = editor.document().borrow();
                (
                    document
                        .current_revision()
                        .map_err(|error| format!("无法读取原文档revision：{error}"))?,
                    document.selection(),
                )
            };
            let expected_revision = if redo || final_check {
                facts.baseline
            } else {
                facts.after.ok_or("缺少原追加版本")?
            };
            let expected_selection = if redo || final_check {
                facts.selection
            } else {
                facts.after_selection.ok_or("缺少原追加选择")?
            };
            if revision != expected_revision || selection != expected_selection {
                return Err("一次Undo/Redo没有恢复完整原root与选择".into());
            }
            if final_check {
                return Ok(());
            } // 最终baseline只读认证，随后真实Return。
            let _payload =
                PayloadUseGuard::enter(&editor.backend.text_view.ivars().payload_use_depth);
            let input =
                ProtocolObject::<dyn NSTextInputClient>::from_ref(editor.backend.text_view());
            if redo {
                // SAFETY: 原正文公开redo:；固定当前代次，返回后只接受预声明下一代。
                unsafe {
                    input.doCommandBySelector(sel!(redo:));
                }
            } else {
                // SAFETY: 原正文公开undo:；固定当前代次，返回后只接受预声明下一代。
                unsafe {
                    input.doCommandBySelector(sel!(undo:));
                }
            }
            let next = expected_generation.checked_add(1).ok_or("历史代次溢出")?;
            if !self.current(surface, &facts, next) {
                return Err("历史动作没有产生预声明下一代".into());
            }
            Ok(())
        })
    }

    pub(crate) fn request_return(&self, failed: bool) -> Result<(), String> {
        POINTER_OPERATION.with_borrow_mut(|slot| *slot = std::rc::Weak::new());
        let operation = self.conflict_operation.borrow_mut().take();
        if let Some(operation) = operation {
            operation.active.set(false);
        }
        CONFLICT_OPERATION.with_borrow_mut(|slot| *slot = std::rc::Weak::new());
        if failed && self.conflict.borrow().is_some() {
            // driver已保存首因时只清自己短夹具的外部覆写；不把恢复字节当冲突处理通过。
            let (document, path, text) = {
                let conflict = self.conflict.borrow();
                let conflict = conflict.as_ref().unwrap();
                (
                    conflict.current_document.clone(),
                    conflict.current_path,
                    conflict.current_text,
                )
            };
            with_active_native_markdown_surface(|surface| {
                let editor = surface
                    .editor
                    .as_ref()
                    .ok_or("失败夹具清理缺少原Doc，不能回写磁盘")?;
                if surface.current_note_relative_path.as_deref() != Some(path)
                    || !WeakMarkdownDocument::ptr_eq(&document, &Rc::downgrade(editor.document()))
                {
                    return Err("失败夹具清理的Doc Weak/path已失效，不能回写磁盘".into());
                }
                Self::verify_conflict_text(&editor.document().borrow(), text)
            })?;
            // 原unsafe验收入口只接调用者独占TempDir；path是上面两个固定相对字面值之一。
            let target = self.path.parent().unwrap().join(path);
            std::fs::write(target, text).map_err(|cause| cause.to_string())?;
            if path == "工作负载.md" {
                self.conflict
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .original_disk_text = text;
            }
            println!(
                "native_conflict_failure_fixture_cleanup original_failure_preserved=true fixed_synthetic_bytes_only=true conflict_acceptance=false"
            );
        }
        request_native_markdown_window_exit()
    }

    pub(crate) fn finish(&self) -> Result<(), String> {
        if !self.lifecycle.valid.get()
            || self.lifecycle.completed.get().is_none()
            || self.lifecycle.completed.get() != self.lifecycle.accepted.get()
            || ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(Option::is_some)
            || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get).is_some()
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
        {
            return Err("追加工作负载缺少真实唯一strict close".into());
        }
        let facts = self.facts()?;
        if facts.document.upgrade().is_some() {
            return Err("追加工作负载关闭后正文仍活".into());
        }
        let pointer = self.pointer_operation.borrow_mut().take();
        if let Some(pointer) = pointer {
            let view = pointer.view.load();
            let view_alive = view.is_some();
            // 原正式角色允许框架仍持该 view，但必须完整清空业务容量/草稿/事件槽和公开输入状态。
            // 本探针不能把新增 Weak-nil 条件冒充原释放门，也不能只凭 sealed 标志接受旧正文。
            let view_payload_empty = view.as_ref().is_none_or(|view| view.payload_is_empty());
            let view_failure = view
                .as_ref()
                .filter(|_| !view_payload_empty)
                .map(|view| view.payload_failure_diagnostic());
            drop(view); // 本次观察的 Native claim 先归还；不让探针自己延长 view 到终点。
            let pointer_alive = pointer
                .pointer
                .borrow()
                .as_ref()
                .is_some_and(|value| value.upgrade().is_some());
            println!(
                "native_table_pointer_close view_alive={view_alive} view_payload_empty={view_payload_empty} pointer_alive={pointer_alive} formal=false"
            );
            if let Some(failure) = view_failure {
                return Err(format!(
                    "真实 strict close 后原鼠标 view 仍保留业务数据：{failure}"
                ));
            }
            if pointer_alive {
                return Err("真实 strict close 后原鼠标 Pointer 仍活".into());
            }
        } // 原 Pointer 必须实际释放；该既有 view 复用正式逐角色空载荷合同，不声称已销毁或通过内存门槛。
        let stale = self.stale_pointer.borrow_mut().take();
        if stale.is_some_and(|pointer| pointer.upgrade().is_some()) {
            return Err("真实关闭后被撤销的原鼠标 Pointer 仍活".into());
        }
        let information = self.information_root.borrow_mut().take();
        if information
            .as_ref()
            .is_some_and(|root| root.load().is_some())
        {
            return Err("真实strict关闭后原信息卡仍活".into());
        }
        drop(information); // 只有原Weak；卡片所有实际角色仍须通过已有完整session账本。

        let conflict_result = self.conflict.borrow().as_ref().map(|conflict| {
            (
                conflict.current_document.clone(),
                conflict.bar.clone(),
                conflict.original_disk_text,
                conflict.completed,
            )
        });
        let had_conflict = conflict_result.is_some();
        let conflict_completed =
            if let Some((document, bar, original_disk_text, completed)) = conflict_result {
                // 上面的事实借用已经结束；真实Weak升级和文件读取不跨RefCell借用。
                if document.strong_count() != 0 || bar.load().is_some() {
                    return Err("冲突往返最终Doc或原冲突条没有真实释放".into());
                }
                Self::verify_conflict_file(&self.path, original_disk_text)?;
                let copy = self.path.parent().unwrap().join(CONFLICT_COPY_PATH);
                match std::fs::metadata(&copy) {
                    Ok(_) => Self::verify_conflict_file(&copy, CONFLICT_DRAFT)?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound && !completed => {}
                    Err(error) => return Err(error.to_string()),
                }
                completed
            } else {
                false
            };
        let find_replaced = self.case == NativeMarkdownTableAppendCase::FindReplacementComposition;
        let formatted = matches!(
            self.case,
            NativeMarkdownTableAppendCase::FormatSelection
                | NativeMarkdownTableAppendCase::ToolbarFormatSelection
                | NativeMarkdownTableAppendCase::ToolbarFieldHistory
        );
        if !had_conflict {
            let saved = MarkdownDocumentBuffer::open(&self.path, 1024 * 1024)
                .map_err(|_| "无法核对合成小表最终字节")?;
            let expected_saved = if formatted || find_replaced {
                facts.after.ok_or("格内格式缺少实际保存后的版本")? // 该场景最后明确保存格式结果，不回退成原源。
            } else {
                facts.baseline // 原追加历史场景仍须回到最初文件字节。
            };
            if saved
                .current_revision()
                .map_err(|error| format!("无法读取原文档revision：{error}"))?
                != expected_saved
            {
                return Err("合成小表最终字节未回到baseline".into());
            }
            drop(saved);
        }
        self.workspace.close_session();
        self.receipt.borrow_mut().take(); // 仅纯Weak/标量收据，strict已完成。
        self.activation_receipt.borrow_mut().take();
        let field_history = self.field_history_editor.borrow_mut().take();
        drop(field_history); // 原editor Weak在借用外归还；关闭释放授权仍来自既有完整账本。
        let find = self.find_replacement.borrow_mut().take();
        if find
            .as_ref()
            .is_some_and(|probe| probe.bar.upgrade().is_some())
        {
            return Err("真实 strict close 后原查找栏 owner 仍活".into());
        }
        drop(find); // 只归还原栏和字段 Weak；共享 editor 的完整空载荷与字段关系仍服从原正式关闭合同。
        println!(
            "native_table_append_strict_close passed=true baseline_bytes={} saved_formatted_bytes={} conflict_roundtrip={} physical_tab=false",
            !(formatted || find_replaced),
            formatted && !had_conflict,
            conflict_completed,
        );
        Ok(())
    }
}
