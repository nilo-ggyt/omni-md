//! view拥有的有限折叠状态；storage只保Rust Weak，观察者没有Native对象或正文所有权。

use super::selection_state::{
    NativeMarkdownSelectionInstallObservation, NativeMarkdownSelectionState,
    NativeMarkdownVerticalGoal,
};
use super::*;
use omni_markdown_core::markdown_folding::{
    MarkdownFoldRegion,
    anchor_mapping::{MarkdownFoldAnchorMapping, MarkdownFoldChange, MarkdownFoldMappingError},
};
use omni_markdown_core::{MarkdownPreparedText, MarkdownTextChange, MarkdownTextChangeObserver};
use std::ops::Deref;

/// 只在显式折叠metadata的固定预算内求联合覆盖；无序、嵌套与重叠不改变隐藏集合。
fn hidden_fold_union_covers(
    gap: &std::ops::Range<usize>,
    length: usize,
    folds: &[MarkdownFoldRegion],
) -> bool {
    use omni_markdown_core::markdown_folding::anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS;
    if gap.start >= gap.end
        || gap.end > length
        || folds.len() > MAX_MARKDOWN_FOLD_ANCHORS
        || folds.iter().any(|fold| {
            fold.hidden_utf16_range.start >= fold.hidden_utf16_range.end
                || fold.hidden_utf16_range.end > length
        })
    {
        return false; // 非法查询/metadata或预算错误不能被另一有效区间掩盖。
    }
    // 固定栈scratch只复制最多256对端点，排序后线性合并；不复制正文，也不按gap长度扫描。
    let mut ordered = [const { 0..0 }; MAX_MARKDOWN_FOLD_ANCHORS];
    for (slot, fold) in ordered.iter_mut().zip(folds) {
        *slot = fold.hidden_utf16_range.clone();
    }
    let ordered = &mut ordered[..folds.len()];
    ordered.sort_unstable_by_key(|range| (range.start, range.end));
    let mut frontier = gap.start;
    for hidden in ordered {
        if hidden.start > frontier {
            return false; // 已排序，后续区间也不能补回这个未隐藏单元。
        }
        if hidden.end > frontier {
            frontier = hidden.end.min(gap.end);
            if frontier == gap.end {
                return true;
            }
        }
    }
    false // 没有折叠或联合未覆盖尾端，不能借普通正文填满缺口。
}

/// 原 Core 坐标只到末行内容结尾；Native delegate 却按起点在 hidden 内的整 paragraph 省略。
/// 只补证末行真实终止符，不修改 Core fold、正文或历史；最多请求两个 UTF-16 单元。
fn native_fold_projection_end(
    hidden: &std::ops::Range<usize>,
    length: usize,
    mut unit_at: impl FnMut(usize) -> Option<u16>,
    physical_line: impl FnOnce(usize) -> Option<(usize, usize)>,
) -> Option<usize> {
    if hidden.start >= hidden.end || hidden.end > length {
        return None; // 非法 metadata 不能凭邻近换行获得新的隐藏权限。
    }
    if hidden.end == length {
        return Some(length); // EOF 无终止单元，既不读取越界位置，也不做 end+1 猜测。
    }
    let ending = unit_at(hidden.end)?;
    if !matches!(ending, 10 | 13) {
        return Some(hidden.end); // 普通正文、NEL、LS、PS 都不作为 Core 物理行尾的额外授权。
    }
    let previous = unit_at(hidden.end - 1)?;
    if matches!(previous, 10 | 13 | 0x2029) {
        // 若前一单元已结束 paragraph，end 处是另一个空 paragraph 的起点，恰好不在 hidden 内。
        // 它不能因为相邻或值也是换行而被 Native 整段省略规则一并隐藏。
        return Some(hidden.end);
    }
    let (line_start, next_line_start) = physical_line(hidden.end)?;
    if !hidden.contains(&line_start) {
        return Some(hidden.end); // 最后一段的实际起点不在 hidden 内，不能补其 terminator。
    }
    let units = next_line_start.checked_sub(hidden.end)?;
    if next_line_start > length || !(units == 1 || (ending == 13 && units == 2)) {
        return None; // 同一 canonical CRLF 聚合只允许 LF/CR 的1单元或 CRLF 的2单元终止。
    }
    Some(next_line_start)
}

/// 先保留原 metadata 联合判断；确实缺少尾 terminator 时才在同一 Doc 借用内补有限源证明。
fn native_hidden_fold_union_covers(
    gap: &std::ops::Range<usize>,
    document: &MarkdownDocumentBuffer,
    folds: &[MarkdownFoldRegion],
) -> bool {
    use omni_markdown_core::markdown_folding::anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS;
    let length = document.utf16_len();
    if hidden_fold_union_covers(gap, length, folds) {
        return true; // 原隐藏坐标已经足够，不为日志或投影重复读取字符。
    }
    if gap.start >= gap.end || gap.end > length || folds.len() > MAX_MARKDOWN_FOLD_ANCHORS {
        return false; // 保持原非法查询与 metadata 预算标准，先拒绝再读任何末端。
    }
    let mut projected = [const {
        MarkdownFoldRegion {
            header_line: 0,
            hidden_utf16_range: 0..0,
        }
    }; MAX_MARKDOWN_FOLD_ANCHORS];
    for (slot, fold) in projected.iter_mut().zip(folds) {
        let end = native_fold_projection_end(
            &fold.hidden_utf16_range,
            length,
            |offset| document.utf16_code_unit_at(offset).ok(),
            |offset| {
                // 物理行索引复用同一 piece tree 的 CR/LF/CRLF 聚合，不扫描该 fold 的完整正文。
                // 索引可解码既有有界叶；显式字符请求仍严格最多上方两单元。
                let byte = document.byte_offset_for_utf16(offset).ok()?;
                let number = document.line_for_byte(byte).ok()?;
                let start = document
                    .utf16_offset_for_byte(document.byte_offset_for_line(number).ok()?)
                    .ok()?;
                let next = number.checked_add(1)?;
                let end = if next < document.line_count() {
                    document
                        .utf16_offset_for_byte(document.byte_offset_for_line(next).ok()?)
                        .ok()?
                } else {
                    length // 最后一个 CR/LF 仍只到真实 EOF，不虚构后继空单元。
                };
                Some((start, end))
            },
        );
        let Some(end) = end else {
            return false;
        }; // 缺源/非法末端不能用另一个 fold 掩盖证明缺失。
        *slot = MarkdownFoldRegion {
            header_line: fold.header_line,
            hidden_utf16_range: fold.hidden_utf16_range.start..end,
        }; // 仅栈内投影副本；原 ranges、revision、selection 与历史完全不变。
    }
    hidden_fold_union_covers(gap, length, &projected[..folds.len()])
}

#[derive(Debug, thiserror::Error)]
pub(super) enum NativeMarkdownFoldError {
    #[error("折叠对应的笔记、模式或输入状态已变更")]
    Stale,
    #[error("无法确认多重选择的主端点，请先使用单一文本选择")]
    UnresolvedSelection(NativeMarkdownSelectionCapture),
    #[error("原生文本选择坐标或方向无效")]
    InvalidSelection,
    #[error("{0}")]
    Mapping(#[from] MarkdownFoldMappingError),
    #[error("{0}")]
    Document(#[from] omni_markdown_core::MarkdownDocumentBufferError),
}

/// 透明复用原fold Vec借用接口；新共享边只指向这些有限纯metadata。
pub(super) struct NativeMarkdownFoldState {
    ranges: RefCell<Vec<MarkdownFoldRegion>>,
    pub(super) generation: Cell<u64>,
    revision: Cell<u64>,
    owner: Cell<usize>,
    mode: Cell<MarkdownPresentationMode>,
    sealed: Cell<bool>,
    document: WeakMarkdownDocument,
    delete_depth: Cell<usize>,
    composition: Cell<bool>,
    layout_dirty: Cell<bool>,
    selection: NativeMarkdownSelectionState,
}

impl Deref for NativeMarkdownFoldState {
    type Target = RefCell<Vec<MarkdownFoldRegion>>;
    fn deref(&self) -> &Self::Target {
        &self.ranges
    }
}

impl NativeMarkdownFoldState {
    pub(super) fn new(document: &SharedMarkdownDocument, mode: MarkdownPresentationMode) -> Self {
        let (generation, selection) = {
            // 初始化只借一次Core；方向事实与同一文档代次绑定，不建立另一个正文owner。
            let document = document.borrow();
            (document.generation(), document.selection())
        };
        Self {
            ranges: RefCell::new(Vec::new()),
            generation: Cell::new(generation),
            revision: Cell::new(0),
            owner: Cell::new(0),
            mode: Cell::new(mode),
            sealed: Cell::new(false),
            document: Rc::downgrade(document),
            delete_depth: Cell::new(0),
            composition: Cell::new(false),
            layout_dirty: Cell::new(false),
            selection: NativeMarkdownSelectionState::new(generation, selection),
        }
    }
    pub(super) fn matches_document(&self, document: &SharedMarkdownDocument) -> bool {
        std::rc::Weak::ptr_eq(&self.document, &Rc::downgrade(document))
    }
    pub(super) fn revision(&self) -> u64 {
        self.revision.get()
    }
    pub(super) fn read_is_current(&self, revision: u64) -> bool {
        self.revision.get() == revision && self.owner_is_current()
    }
    /// 原票据证明同 owner、代次、fold revision 与选择；只在其完整借用内解释实际 Native 空洞。
    /// Core metadata 不够时，每 fold 最多补读两单元并核物理行聚合，不复制或解析整段正文。
    pub(super) fn hidden_range_is_covered(
        &self,
        gap: &std::ops::Range<usize>,
        generation: u64,
        revision: u64,
        original_current: &impl Fn() -> bool,
    ) -> bool {
        if !original_current()
            || !self.read_is_current(revision)
            || self.generation.get() != generation
        {
            return false;
        }
        let covered = (|| {
            let document = self.document.upgrade()?;
            let document = document.try_borrow().ok()?;
            if document.generation() != generation {
                return None;
            }
            let folds = self.ranges.try_borrow().ok()?;
            Some(native_hidden_fold_union_covers(gap, &document, &folds))
        })()
        .unwrap_or(false);
        // current可包含Native几何getter；必须先结束上述Doc/fold借用和临时正文claim。
        covered
            && original_current()
            && self.read_is_current(revision)
            && self.generation.get() == generation
    }
    pub(super) fn set_composition(&self, composing: bool) {
        self.composition.set(composing);
    }
    pub(super) fn take_layout_dirty(&self) -> bool {
        self.layout_dirty.replace(false)
    }
    pub(super) fn bind_owner(&self, owner: usize) {
        if self.owner.replace(owner) != owner {
            // owner身份切换不继承旧安装权限，即使恰好绑定同一正文坐标。
            self.selection.revoke_install();
        }
    }
    pub(super) fn set_mode(&self, mode: MarkdownPresentationMode) {
        if self.mode.replace(mode) != mode {
            // 逻辑方向仍是Core事实；换行side/目标列与安装lease不跨模式几何复用。
            self.selection.clear_vertical_goal();
            self.advance();
        }
    }
    pub(super) fn advance(&self) {
        self.selection.discard_install();
        self.revision.set(
            self.revision
                .get()
                .checked_add(1)
                .expect("fold revision overflow"),
        );
    }
    pub(super) fn synchronize_generation(&self, generation: u64) {
        if self.generation.replace(generation) != generation {
            // 未经本adapter提交的新代次没有旧隐藏坐标权限；保完整原文可读。
            let had_folds = !self.ranges.borrow().is_empty();
            self.ranges.borrow_mut().clear();
            self.selection.reset(generation);
            if had_folds {
                self.layout_dirty.set(true); // 旧fragment也须由既有完成事件失效，不能仅清metadata。
            }
            self.advance();
        }
    }
    pub(super) fn seal(&self) {
        // 关闭释放容量而非仅clear；系统缓存只能留下无业务payload的纯空壳。
        self.sealed.set(true);
        self.owner.set(0);
        self.composition.set(false);
        self.layout_dirty.set(false);
        self.selection.seal();
        self.advance();
        let previous = self.ranges.replace(Vec::new());
        drop(previous);
    }
    pub(super) fn owner_is_current(&self) -> bool {
        if self.sealed.get()
            || self.owner.get() == 0
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
            || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
        {
            return false;
        }
        let owner = self.owner.get();
        let active = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| {
            surface.as_ref().is_some_and(|surface| {
                std::ptr::from_ref(&*surface.controller).addr() == owner
                    && surface.closing_request.is_none()
                    && !surface.controller.ivars().close_finalizer_scheduled.get()
            })
        });
        // exact借出owner的payload guard阻止accepted close；sealed/VERIFYING/Closing也独立拒绝。
        active || BORROWED_NATIVE_MARKDOWN_OWNER.with(Cell::get) == Some(owner)
    }
    pub(super) fn capture_selection(
        self: &Rc<Self>,
    ) -> Result<NativeMarkdownSelectionCapture, NativeMarkdownFoldError> {
        if !self.owner_is_current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let document = self
            .document
            .upgrade()
            .ok_or(NativeMarkdownFoldError::Stale)?;
        let (generation, selection_before, utf16_len) = {
            let document = document.borrow();
            (
                document.generation(),
                document.selection(),
                document.utf16_len(),
            )
        };
        self.synchronize_generation(generation);
        Ok(NativeMarkdownSelectionCapture {
            state: Rc::clone(self),
            ticket: FoldTicket {
                document: self.document.clone(),
                generation,
                revision: self.revision.get(),
                owner: self.owner.get(),
                mode: self.mode.get(),
                selection_serial: self.selection.serial(),
                selection_before,
                utf16_len,
            },
        })
    }
    pub(super) fn select_head(&self, head: usize) {
        let mut ranges = self.ranges.borrow_mut();
        let before = ranges.len();
        ranges.retain(|range| {
            !(head > range.hidden_utf16_range.start && head < range.hidden_utf16_range.end)
        });
        if ranges.len() != before {
            self.advance();
            self.layout_dirty.set(true);
        }
    }

    /// 完整方向只取同代Core事实；调用方还须核Native真实有序范围，不能仅凭此认定已显示。
    pub(super) fn known_selection(&self) -> Option<MarkdownTextSelection> {
        if !self.owner_is_current() {
            return None;
        }
        let document = self.document.upgrade()?;
        let document = document.borrow();
        self.selection
            .known_selection(document.generation(), document.selection())
    }

    pub(super) fn known_head_is_upstream(&self) -> Option<bool> {
        let document = self.document.upgrade()?;
        let document = document.borrow();
        self.owner_is_current().then(|| {
            self.selection
                .known_head_is_upstream(document.generation(), document.selection())
        })?
    }

    #[cfg(test)]
    pub(super) fn known_vertical_goal(&self) -> Option<NativeMarkdownVerticalGoal> {
        let document = self.document.upgrade()?;
        let document = document.borrow();
        self.owner_is_current().then(|| {
            self.selection
                .known_vertical_goal(document.generation(), document.selection())
        })?
    }

    /// 外来非空选择未知时撤销旧事实；后继动作不能把旧head贴到当前Native范围上。
    pub(super) fn reset_selection(&self) {
        self.selection.reset(self.generation.get());
        self.advance();
    }

    /// 每个真实新输入入口撤旧lease/capture；保留Core方向供该新动作重新捕获。
    pub(super) fn invalidate_native_install(&self) {
        self.selection.revoke_install();
    }

    /// delegate只可抑制原收据声明的before/after；第三个范围撤旧权限并交回正常输入。
    pub(super) fn observe_native_selection(
        &self,
        range: NSRange,
    ) -> Option<NativeMarkdownSelectionInstallObservation> {
        let observation = self.selection.observe_install(range)?;
        let core_is_current = self.document.upgrade().is_some_and(|document| {
            let document = document.borrow();
            self.selection
                .install_core_is_current(document.generation(), document.selection())
        });
        if !self.owner_is_current()
            || !core_is_current
            || observation == NativeMarkdownSelectionInstallObservation::Foreign
        {
            // 不接纳未知通知为同动作中间态，也不让旧guard继续覆盖新输入。
            self.reset_selection();
            return Some(NativeMarkdownSelectionInstallObservation::Foreign);
        }
        Some(observation)
    }

    pub(super) fn prepare(
        self: &Rc<Self>,
        document: &MarkdownDocumentBuffer,
        explicit_after: bool,
    ) -> Result<NativeMarkdownFoldObserver, NativeMarkdownFoldError> {
        if !self.owner_is_current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        self.synchronize_generation(document.generation());
        let mapper = MarkdownFoldAnchorMapping::new(
            document.utf16_len(),
            &self.ranges.borrow(),
            self.delete_depth.get() != 0,
        )?;
        Ok(NativeMarkdownFoldObserver {
            state: Rc::clone(self),
            ticket: FoldTicket {
                document: self.document.clone(),
                generation: document.generation(),
                revision: self.revision.get(),
                owner: self.owner.get(),
                mode: self.mode.get(),
                selection_serial: self.selection.serial(),
                selection_before: document.selection(),
                utf16_len: document.utf16_len(),
            },
            mapper,
            explicit_after,
            native_before_range: ordered_selection_range(document.selection()),
        })
    }
    pub(super) fn begin_delete(&self) -> NativeMarkdownDeleteScope<'_> {
        let active = !self.composition.get();
        if active {
            self.delete_depth.set(
                self.delete_depth
                    .get()
                    .checked_add(1)
                    .expect("delete input scope overflow"),
            );
        }
        NativeMarkdownDeleteScope(self, active)
    }
}

pub(super) struct NativeMarkdownDeleteScope<'a>(&'a NativeMarkdownFoldState, bool);
impl Drop for NativeMarkdownDeleteScope<'_> {
    fn drop(&mut self) {
        if self.1 {
            self.0.delete_depth.set(self.0.delete_depth.get() - 1);
        }
    }
}

#[derive(Clone)]
struct FoldTicket {
    document: WeakMarkdownDocument,
    generation: u64,
    revision: u64,
    owner: usize,
    mode: MarkdownPresentationMode,
    selection_serial: u64,
    selection_before: MarkdownTextSelection,
    // 首个 Native 调用前同 root 的精确长度；提交后票据会更新为候选长度，不能沿用旧长度。
    utf16_len: usize,
}
impl FoldTicket {
    fn current(&self, state: &NativeMarkdownFoldState) -> bool {
        // 此处不能borrow RefCell<Doc>：Core finish正在持有真实mutable document借用。
        // Doc代次在adapter进入Core前核准，之后纯Rust独占栈没有Native重入点。
        std::rc::Weak::ptr_eq(&self.document, &state.document)
            && self.generation == state.generation.get()
            && self.revision == state.revision.get()
            && self.owner == state.owner.get()
            && self.mode == state.mode.get()
            && self.selection_serial == state.selection.serial()
            && state.owner_is_current()
    }
}

/// Native getter之前就取得原权限，返回时不重新领取较新的owner/revision。
#[derive(Clone)]
pub(super) struct NativeMarkdownSelectionCapture {
    state: Rc<NativeMarkdownFoldState>,
    ticket: FoldTicket,
}
impl std::fmt::Debug for NativeMarkdownSelectionCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeMarkdownSelectionCapture") // 不输出Native地址、正文或路径。
    }
}

impl NativeMarkdownSelectionCapture {
    pub(super) fn current(&self) -> bool {
        self.ticket.current(&self.state)
            && self.ticket.document.upgrade().is_some_and(|document| {
                let document = document.borrow();
                document.generation() == self.ticket.generation
                    && document.selection() == self.ticket.selection_before
                    && document.utf16_len() == self.ticket.utf16_len
            })
    }
    pub(super) fn finish(
        self,
        selection: MarkdownTextSelection,
        proven_head: bool,
    ) -> Result<NativeMarkdownCapturedSelection, NativeMarkdownFoldError> {
        if !self.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let head_is_upstream = self
            .state
            .selection
            .known_head_is_upstream(self.ticket.generation, selection);
        let vertical_goal = self
            .state
            .selection
            .known_vertical_goal(self.ticket.generation, selection);
        let native_before_range = ordered_selection_range(self.ticket.selection_before);
        Ok(NativeMarkdownCapturedSelection {
            selection,
            capture: self,
            proven_head,
            head_is_upstream,
            vertical_goal,
            native_before_range,
        })
    }
}
#[derive(Clone)]
pub(super) struct NativeMarkdownCapturedSelection {
    pub(super) selection: MarkdownTextSelection,
    capture: NativeMarkdownSelectionCapture,
    pub(super) proven_head: bool,
    head_is_upstream: Option<bool>,
    vertical_goal: Option<NativeMarkdownVerticalGoal>,
    native_before_range: NSRange,
}
impl NativeMarkdownCapturedSelection {
    /// 只借用这份原票，不重新捕获当前代次；用于同一同步输入内的实际有序查询。
    pub(super) fn native_capture(&self) -> &NativeMarkdownSelectionCapture {
        &self.capture
    }

    /// 无字符 bookkeeping 只声明参数派生的临时范围与原 Core 范围，不推进 serial 或重签选择。
    pub(super) fn begin_bookkeeping_install(
        &self,
        temporary_range: NSRange,
    ) -> Result<NativeMarkdownSelectionInstallGuard, NativeMarkdownFoldError> {
        if !self.current()
            || !self.proven_head
            || self.selection != self.capture.ticket.selection_before
        {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let end = temporary_range
            .location
            .checked_add(temporary_range.length)
            .ok_or(NativeMarkdownFoldError::InvalidSelection)?;
        let document = self
            .capture
            .ticket
            .document
            .upgrade()
            .ok_or(NativeMarkdownFoldError::Stale)?;
        {
            let document = document.borrow();
            document.byte_offset_for_utf16(temporary_range.location)?;
            document.byte_offset_for_utf16(end)?;
        }
        let id = self
            .capture
            .state
            .selection
            .begin_install(
                self.capture.ticket.generation,
                self.capture.ticket.selection_serial,
                temporary_range,
                self.selection,
            )
            .ok_or(NativeMarkdownFoldError::Stale)?;
        Ok(NativeMarkdownSelectionInstallGuard {
            capture: self.capture.clone(),
            id,
        })
    }

    pub(super) fn belongs_to(&self, document: &SharedMarkdownDocument) -> bool {
        self.capture.state.matches_document(document)
    }
    pub(super) fn current_in(&self, document: &MarkdownDocumentBuffer) -> bool {
        self.capture.ticket.generation == document.generation()
            && self.capture.ticket.selection_before == document.selection()
            && self.capture.ticket.utf16_len == document.utf16_len()
            && self.capture.ticket.current(&self.capture.state)
    }

    /// 保持原capture/serial纯核；Native导航返回后不能重新领取较新的权限。
    pub(super) fn current(&self) -> bool {
        self.capture.current()
    }

    /// 导航只更换原票据内的目标；两端必须仍属于同一Core代次的合法scalar边界。
    pub(super) fn with_selection(
        mut self,
        next: MarkdownTextSelection,
        proven_head: bool,
    ) -> Result<Self, NativeMarkdownFoldError> {
        if !self.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let document = self
            .capture
            .ticket
            .document
            .upgrade()
            .ok_or(NativeMarkdownFoldError::Stale)?;
        {
            let document = document.borrow();
            document.byte_offset_for_utf16(next.anchor_utf16)?;
            document.byte_offset_for_utf16(next.head_utf16)?;
        }
        if next != self.selection {
            // 新目标的视觉side和列必须由本次导航明确交付，不能从旧head默认继承。
            self.head_is_upstream = None;
            self.vertical_goal = None;
        }
        self.selection = next;
        self.proven_head = proven_head;
        Ok(self)
    }

    pub(super) fn head_is_upstream(&self) -> Option<bool> {
        self.head_is_upstream
    }

    pub(super) fn vertical_goal(&self) -> Option<NativeMarkdownVerticalGoal> {
        self.vertical_goal
    }

    pub(super) fn with_head_side(mut self, side: Option<bool>) -> Self {
        self.head_is_upstream = side;
        self
    }

    pub(super) fn with_vertical_goal(mut self, goal: Option<NativeMarkdownVerticalGoal>) -> Self {
        self.vertical_goal = goal.filter(|goal| goal.is_finite());
        self
    }

    /// 原票内实际观察的单一Native范围独立于Core before；未知选择恢复时不能重写原Core权限。
    pub(super) fn with_observed_native_before(
        mut self,
        range: NSRange,
    ) -> Result<Self, NativeMarkdownFoldError> {
        if !self.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownFoldError::InvalidSelection)?;
        if range.location == objc2_foundation::NSNotFound as usize {
            // NSNotFound不是观测到的单范围，不能被折算成EOF或默认caret。
            return Err(NativeMarkdownFoldError::InvalidSelection);
        }
        let document = self
            .capture
            .ticket
            .document
            .upgrade()
            .ok_or(NativeMarkdownFoldError::Stale)?;
        {
            let document = document.borrow();
            document.byte_offset_for_utf16(range.location)?;
            document.byte_offset_for_utf16(end)?;
        }
        self.native_before_range = range;
        Ok(self)
    }

    /// 同一纯发布中推进fold与方向serial，再签发本次转换收据；旧capture始终失效。
    pub(super) fn publish_head(&self, selection_changed: bool) -> NativeMarkdownCommittedSelection {
        assert!(self.capture.ticket.current(&self.capture.state));
        if selection_changed {
            self.capture.state.advance();
        } // A→B→A选择也撤销早期getter票据。
        if self.proven_head {
            self.capture.state.select_head(self.selection.head_utf16);
        }
        let serial = self.capture.state.selection.publish(
            self.capture.ticket.generation,
            self.selection,
            self.proven_head,
            self.head_is_upstream,
            self.vertical_goal,
        );
        let before_range = self.native_before_range;
        let mut ticket = self.capture.ticket.clone();
        ticket.revision = self.capture.state.revision.get();
        ticket.selection_serial = serial;
        ticket.selection_before = self.selection;
        NativeMarkdownCommittedSelection {
            capture: NativeMarkdownSelectionCapture {
                state: Rc::clone(&self.capture.state),
                ticket,
            },
            selection: self.selection,
            before_range,
            native_before_utf16_len: self.capture.ticket.utf16_len,
            #[cfg(feature = "native-markdown-runtime-probe")]
            source_ticket: self.capture.ticket.clone(),
        }
    }
}

pub(super) struct NativeMarkdownFoldObserver {
    state: Rc<NativeMarkdownFoldState>,
    ticket: FoldTicket,
    mapper: MarkdownFoldAnchorMapping,
    explicit_after: bool,
    native_before_range: NSRange,
}
impl NativeMarkdownFoldObserver {
    /// 只接收同一原票观察到的 Native before；Core ticket 的完整选择仍独立核权。
    pub(super) fn with_native_before(
        mut self,
        captured: &NativeMarkdownCapturedSelection,
    ) -> Result<Self, NativeMarkdownFoldError> {
        if !self.accepts_selection(captured) || !captured.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        self.native_before_range = captured.native_before_range;
        Ok(self)
    }

    pub(super) fn accepts_selection(&self, selection: &NativeMarkdownCapturedSelection) -> bool {
        let before = &selection.capture.ticket;
        Rc::ptr_eq(&self.state, &selection.capture.state)
            && before.generation == self.ticket.generation
            && before.revision == self.ticket.revision
            && before.owner == self.ticket.owner
            && before.mode == self.ticket.mode
            && before.selection_serial == self.ticket.selection_serial
            && before.selection_before == self.ticket.selection_before
            && before.utf16_len == self.ticket.utf16_len
            && std::rc::Weak::ptr_eq(&before.document, &self.ticket.document)
    }
    pub(super) fn is_current(&self, document: &MarkdownDocumentBuffer) -> bool {
        self.ticket.generation == document.generation()
            && self.ticket.selection_before == document.selection()
            && self.ticket.utf16_len == document.utf16_len()
            && self.ticket.current(&self.state)
    }
}

/// Core成功后唯一纯安装载荷；不借Native、不再读源，也不携完整变化流。
pub(super) struct NativeMarkdownPreparedFolds {
    state: Rc<NativeMarkdownFoldState>,
    ticket: FoldTicket,
    ranges: Vec<MarkdownFoldRegion>,
    selection: MarkdownTextSelection,
    native_before_range: NSRange,
    // observer.finish 从实际候选 root 取得；发布后 capture 必须使用新长度，旧长度另存原生来源。
    utf16_len_after: usize,
}
impl NativeMarkdownPreparedFolds {
    /// 两阶段 Core 候选尚未发布时，原 fold 票与原完整 Core 选择必须仍然成立。
    pub(super) fn is_current(&self, document: &MarkdownDocumentBuffer) -> bool {
        self.ticket.current(&self.state)
            && self.ticket.generation == document.generation()
            && self.ticket.selection_before == document.selection()
            && self.ticket.utf16_len == document.utf16_len()
    }

    pub(super) fn publish(mut self, generation: u64) -> NativeMarkdownCommittedSelection {
        // finish与此处之间只有Core日志封页/root提交和Rust借用结束，没有Native调用或可并行Rc写者。
        assert!(
            self.ticket.current(&self.state),
            "pure fold publication lost its precommit authority"
        );
        let previous = self.state.ranges.replace(self.ranges);
        self.state.generation.set(generation);
        if !previous.is_empty() || !self.state.ranges.borrow().is_empty() {
            self.state.layout_dirty.set(true);
        }
        self.state.advance();
        let serial = self
            .state
            .selection
            .publish(generation, self.selection, true, None, None);
        drop(previous);
        // 这是唯一能签发提交后票据的位置：原root刚发布，尚无Native通知或局部池drain。
        let native_before_utf16_len = self.ticket.utf16_len;
        #[cfg(feature = "native-markdown-runtime-probe")]
        let source_ticket = self.ticket.clone(); // 仅观察原转换；不能把它重新发布为产品选择权限。
        self.ticket.generation = generation;
        self.ticket.utf16_len = self.utf16_len_after;
        self.ticket.revision = self.state.revision.get();
        self.ticket.selection_serial = serial;
        let before_range = self.native_before_range;
        self.ticket.selection_before = self.selection;
        NativeMarkdownCommittedSelection {
            capture: NativeMarkdownSelectionCapture {
                state: self.state,
                ticket: self.ticket,
            },
            selection: self.selection,
            before_range,
            native_before_utf16_len,
            #[cfg(feature = "native-markdown-runtime-probe")]
            source_ticket,
        }
    }
}

/// 提交收据的选择后果权限；只携有限纯标量/Weak，不拥有Native或另一个正文root。
pub(super) struct NativeMarkdownCommittedSelection {
    capture: NativeMarkdownSelectionCapture,
    pub(super) selection: MarkdownTextSelection,
    before_range: NSRange,
    native_before_utf16_len: usize,
    #[cfg(feature = "native-markdown-runtime-probe")]
    source_ticket: FoldTicket,
}
impl NativeMarkdownCommittedSelection {
    /// 观察器只认原 capture 的这一次字符后果；同 g/len 或同范围不能替代原 serial 与 root。
    #[cfg(feature = "native-markdown-runtime-probe")]
    pub(super) fn is_text_successor_of(&self, original: &NativeMarkdownSelectionCapture) -> bool {
        let source = &self.source_ticket;
        let before = &original.ticket;
        self.current()
            && Rc::ptr_eq(&self.capture.state, &original.state)
            && std::rc::Weak::ptr_eq(&source.document, &before.document)
            && source.generation == before.generation
            && source.utf16_len == before.utf16_len
            && source.selection_before == before.selection_before
            && source.selection_serial == before.selection_serial
            && source.revision == before.revision
            && source.owner == before.owner
            && source.mode == before.mode
            && source.generation.checked_add(1) == Some(self.capture.ticket.generation)
    }

    /// 首Native之前捕获的原Core范围；调用方不能以安装中读到的新Native值重签声明。
    pub(super) fn native_before_range(&self) -> NSRange {
        self.before_range
    }

    /// 只返回首 Native 前原 root 的长度来源；调用方仍须核同一收据，不能用它读取已提交的新正文。
    pub(super) fn native_before_utf16_len(&self) -> usize {
        self.native_before_utf16_len
    }

    /// 读取原提交文档的精确长度只需要原票仍有效；不要求或补发逻辑 head 证据。
    pub(super) fn current_utf16_len_for(&self, document: &SharedMarkdownDocument) -> Option<usize> {
        // 先核原 Weak 身份、owner/mode/revision/serial，不能以另一份同内容文档替代原 root。
        if !self.capture.state.matches_document(document)
            || !self.capture.ticket.current(&self.capture.state)
            || self.capture.ticket.selection_before != self.selection
        {
            return None;
        }
        // 此处只借用一次 Doc；借用期间不调用会再次升级 Weak 并借文档的 current()。
        let document = document.try_borrow().ok()?;
        if document.generation() != self.capture.ticket.generation
            || document.selection() != self.selection
            || document.utf16_len() != self.capture.ticket.utf16_len
        {
            return None;
        }
        Some(document.utf16_len()) // 纯读取不改变 Known、安装声明或原 serial。
    }

    /// 标量只标识这一份原票；pending 身份还必须同时比较 Rc，绝不以数值领新权限。
    pub(super) fn selection_serial(&self) -> u64 {
        self.capture.ticket.selection_serial
    }

    /// 旧拒绝不能清后继 Known；只在本收据仍 current 的纯栈撤销自己的方向与安装。
    pub(super) fn revoke_pending_selection(&self) {
        if self.current() {
            self.capture.state.reset_selection();
        }
    }

    /// 同提交后果复用原capture；不从当前owner/generation重新领取后继动作权限。
    pub(super) fn captured_selection(
        &self,
    ) -> Result<NativeMarkdownCapturedSelection, NativeMarkdownFoldError> {
        if !self.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        if self
            .capture
            .state
            .selection
            .known_selection(self.capture.ticket.generation, self.selection)
            != Some(self.selection)
        {
            // 纯范围兼容receipt没有完整head证据，不能由这里升级成已证明方向。
            return Err(NativeMarkdownFoldError::UnresolvedSelection(
                self.capture.clone(),
            ));
        }
        self.capture.clone().finish(self.selection, true)
    }

    pub(super) fn current(&self) -> bool {
        self.capture.current() && self.capture.ticket.selection_before == self.selection
    }
    pub(super) fn native_step<T>(
        &self,
        work: impl FnOnce() -> T,
    ) -> Result<T, NativeMarkdownFoldError> {
        if !self.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let result = work();
        if !self.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        Ok(result)
    }

    /// 原收据建立有限安装guard；只clone纯票据/Weak和state，不要求Rc收据或Native owner。
    pub(super) fn begin_native_install(
        &self,
        before_range: NSRange,
    ) -> Result<NativeMarkdownSelectionInstallGuard, NativeMarkdownFoldError> {
        if !self.current() {
            return Err(NativeMarkdownFoldError::Stale);
        }
        if before_range != self.before_range {
            // caller不能把任意有限第三范围签成本次before；实际观测须在原capture内先记录。
            return Err(NativeMarkdownFoldError::InvalidSelection);
        }
        let id = self
            .capture
            .state
            .selection
            .begin_install(
                self.capture.ticket.generation,
                self.capture.ticket.selection_serial,
                before_range,
                self.selection,
            )
            .ok_or(NativeMarkdownFoldError::Stale)?;
        Ok(NativeMarkdownSelectionInstallGuard {
            capture: self.capture.clone(),
            id,
        })
    }

    /// 属性通知前尚未读取Native；使用纯捕获时的原范围建立紧邻栈内lease。
    pub(super) fn begin_pre_notify_install(
        &self,
    ) -> Result<NativeMarkdownSelectionInstallGuard, NativeMarkdownFoldError> {
        self.begin_native_install(self.before_range)
    }
}

/// guard没有Native析构；退出只清自己id/serial，不能清除重入后 successor 的lease。
pub(super) struct NativeMarkdownSelectionInstallGuard {
    capture: NativeMarkdownSelectionCapture,
    id: u64,
}

impl NativeMarkdownSelectionInstallGuard {
    pub(super) fn current(&self) -> bool {
        self.capture.current()
            && self.capture.state.selection.install_matches(
                self.capture.ticket.generation,
                self.capture.ticket.selection_serial,
                self.id,
            )
    }

    pub(super) fn observe(&self, range: NSRange) -> NativeMarkdownSelectionInstallObservation {
        if !self.current() {
            // 原guard已撤权时不触碰新的slot；不能因旧observer迟到清掉后继安装。
            return NativeMarkdownSelectionInstallObservation::Foreign;
        }
        self.capture
            .state
            .observe_native_selection(range)
            .unwrap_or(NativeMarkdownSelectionInstallObservation::Foreign)
    }
}

impl Drop for NativeMarkdownSelectionInstallGuard {
    fn drop(&mut self) {
        self.capture
            .state
            .selection
            .finish_install(self.capture.ticket.selection_serial, self.id);
    }
}

fn ordered_selection_range(selection: MarkdownTextSelection) -> NSRange {
    let start = selection.anchor_utf16.min(selection.head_utf16);
    let end = selection.anchor_utf16.max(selection.head_utf16);
    NSRange::new(start, end - start)
}
impl MarkdownTextChangeObserver for NativeMarkdownFoldObserver {
    type Output = NativeMarkdownPreparedFolds;
    type Error = NativeMarkdownFoldError;
    fn changed(&mut self, change: MarkdownTextChange) -> Result<(), Self::Error> {
        if !self.ticket.current(&self.state) {
            return Err(NativeMarkdownFoldError::Stale);
        }
        self.mapper.push(MarkdownFoldChange {
            old_utf16_range: change.old_utf16_range,
            inserted_utf16_len: change.inserted_utf16_len,
        })?;
        Ok(())
    }
    fn finish(
        self,
        candidate: MarkdownPreparedText<'_>,
        selection_after: MarkdownTextSelection,
    ) -> Result<Self::Output, Self::Error> {
        if !self.ticket.current(&self.state) {
            return Err(NativeMarkdownFoldError::Stale);
        }
        let ranges = self.mapper.finish(
            candidate.utf16_len(),
            self.explicit_after.then_some(selection_after.head_utf16),
        )?;
        let mut prepared = Vec::with_capacity(ranges.len());
        for range in ranges {
            // 只查最多256个候选锚点；header_line来自真正新root，绝不重用旧行号或解析全篇。
            prepared.push(MarkdownFoldRegion {
                header_line: candidate.line_for_utf16(range.start)?,
                hidden_utf16_range: range,
            });
        }
        Ok(NativeMarkdownPreparedFolds {
            state: self.state,
            ticket: self.ticket,
            ranges: prepared,
            selection: selection_after,
            native_before_range: self.native_before_range,
            utf16_len_after: candidate.utf16_len(),
        })
    }
}

#[cfg(test)]
mod tests;
