//! 新正文的纯事务边界：只弱借原文档，不创建TextKit图或负责可见几何。

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

use objc2::runtime::AnyObject;
use objc2_foundation::{NSNotFound, NSRange};
use omni_markdown_core::markdown_document_buffer::MarkdownTextInputComposition;
use omni_markdown_core::markdown_folding::{
    MarkdownFoldRegion,
    anchor_mapping::{MarkdownFoldAnchorMapping, MarkdownFoldChange, MarkdownFoldMappingError},
};
use omni_markdown_core::{
    MarkdownCellDraftError, MarkdownCellDraftInputKind, MarkdownDocumentBuffer,
    MarkdownDocumentBufferError, MarkdownHistoryTextEdit, MarkdownObservedEditError,
    MarkdownPreparedText, MarkdownPreparedTextInput, MarkdownTextChange,
    MarkdownTextChangeObserver, MarkdownTextSelection,
};

mod cell;
mod cell_commit;
mod cell_mutations;
mod formatting;
mod source_commands;
mod target;
pub use cell::NativeMarkdownPagedCellSession;
pub use cell_commit::{
    NativeMarkdownPagedCellCommitError, NativeMarkdownPagedCellCommitResult,
    NativeMarkdownPagedCellPreparedCommit,
};
use target::InputTarget;
pub use target::NativeMarkdownPagedTargetDomain;

use super::selection_state::NativeMarkdownSelectionState;
pub use super::selection_state::{NativeMarkdownBidiCaretSide, NativeMarkdownVerticalGoal};
use super::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES;

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedInputObserverError {
    #[error("原输入派生权限已经撤销")]
    Revoked,
    #[error("无法映射原折叠锚点：{0}")]
    Fold(#[from] MarkdownFoldMappingError),
    #[error("无法核对候选正文中的折叠位置：{0}")]
    Document(#[from] MarkdownDocumentBufferError),
}

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedInputCancellationError {
    #[error("原文档仍在借用，无法确认原候选取消")]
    Borrowed,
    #[error("原候选取消失败：{0}")]
    Document(MarkdownDocumentBufferError),
}

#[derive(Debug, thiserror::Error)]
pub enum NativeMarkdownPagedInputError {
    #[error("原输入或前台权限已经撤销")]
    Revoked,
    #[error("笔记输入已经封闭")]
    Closed,
    #[error("原输入文档已经释放")]
    DocumentReleased,
    #[error("原输入文档或派生状态仍在借用中")]
    Borrowed,
    #[error("另一个原输入事务尚未结束")]
    Busy,
    #[error("该输入票已经被消费")]
    Consumed,
    #[error("输入选择不再属于原完整状态")]
    StateChanged,
    #[error("输入范围越界、溢出或切入Unicode标量")]
    InvalidRange,
    #[error("多范围选择超过上限、次序错误或彼此重叠")]
    InvalidSelectionRanges,
    #[error("多个空选择的删除需要原布局提供真实字素范围")]
    SelectionGeometryRequired,
    #[error("多重选择没有唯一输入替换范围")]
    MultipleSelectionInput,
    #[error("标记文本内的相对选择无效")]
    InvalidMarkedSelection,
    #[error("本次新增输入超过原会话上限")]
    InputTooLarge,
    #[error("输入状态计数已经耗尽")]
    CounterExhausted,
    #[error("输入法仍拥有标记文本，尚不能执行该操作")]
    CompositionActive,
    #[error("无法转换本次原生输入：{0}")]
    NativeInput(String),
    #[error("无法访问原输入文档：{0}")]
    Document(#[from] MarkdownDocumentBufferError),
    #[error("无法访问原单元格草稿：{0}")]
    Cell(#[from] MarkdownCellDraftError),
    #[error("该操作需要原正文目标，不能以单元格局部坐标执行")]
    BodyTargetRequired,
    #[error("无法准备原折叠状态：{0}")]
    Fold(#[from] MarkdownFoldMappingError),
    #[error("原输入预检或历史事务失败：{0}")]
    Edit(#[from] MarkdownObservedEditError<NativeMarkdownPagedInputObserverError>),
    #[error("{cause}；原输入候选取消未获确认：{cleanup}")]
    CancellationUnconfirmed {
        cause: Box<Self>,
        cleanup: NativeMarkdownPagedInputCancellationError,
    },
}

type Result<T> = std::result::Result<T, NativeMarkdownPagedInputError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// canonical历史选择与有限可见选择事实；多选时不能把历史anchor/head冒充当前主方向。
pub struct NativeMarkdownPagedInputState {
    pub generation: u64,
    /// Core原有memento；只有selection_is_directed为true时也代表当前可交互anchor/head。
    pub selection: MarkdownTextSelection,
    pub selection_is_directed: bool,
    pub selection_range_count: usize,
    /// NSTextInputClient的有序主范围；单靠此范围不能恢复多选的未知anchor/head方向。
    pub primary_range: NSRange,
    pub marked: Option<NSRange>,
    pub utf16_len: usize,
    pub selection_serial: u64,
    pub fold_revision: u64,
}

impl NativeMarkdownPagedInputState {
    /// 多范围只知道真实范围集合与主范围，不为导航或查找伪造一个主head。
    #[must_use]
    pub fn directed_selection(self) -> Option<MarkdownTextSelection> {
        self.selection_is_directed.then_some(self.selection)
    }
}

/// 真实回调入口的一张原票；不可Clone，首次尝试后即消费，失败也不能换载荷重试。
pub struct NativeMarkdownPagedInputPermit {
    owner: Weak<NativeMarkdownPagedInput>,
    epoch: u64,
    before: NativeMarkdownPagedInputState,
    consumed: Cell<bool>,
    cell: Option<cell::CellPermit>,
}

#[derive(Clone, Debug)]
/// 仅说明已提交的源范围变化；实际可见区域由后续原票下的布局阶段另行计算。
pub struct NativeMarkdownPagedInputInvalidation {
    pub replaced_utf16_range: Option<Range<usize>>,
    pub change_in_utf16_length: isize,
    pub characters_changed: bool,
    pub selection_changed: bool,
    pub marked_changed: bool,
    pub folds_changed: bool,
}

/// 成功返回即表示Core已发布；后续权限失效只能停止显示后果，不能解释为未发生编辑。
pub struct NativeMarkdownPagedInputCommit {
    owner: Weak<NativeMarkdownPagedInput>,
    epoch: u64,
    pub state: NativeMarkdownPagedInputState,
    pub invalidation: NativeMarkdownPagedInputInvalidation,
}

impl NativeMarkdownPagedInputCommit {
    /// 在View发出任何可重入坐标或显示通知之前验证本次提交后果，绝不重捕后继状态。
    ///
    /// # Errors
    /// 前台、原epoch、输入owner或完整已提交状态变化时拒绝；此错误不会回滚已提交正文。
    pub fn check_current(&self, current: &(impl Fn(u64) -> bool + ?Sized)) -> Result<()> {
        let owner = self
            .owner
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::Closed)?;
        owner.check_original(self.epoch, self.state, current)
    }
}

pub enum NativeMarkdownPagedInputOutcome {
    Committed(NativeMarkdownPagedInputCommit),
    /// 只用于真实无历史或原本没有marked；错误不能借此分支伪装成无变化。
    Unchanged(NativeMarkdownPagedInputState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedInputHistory {
    Undo,
    Redo,
}

/// composition root持唯一Rc；内部只有原Doc弱引用与有限派生metadata，不保活任何源映射。
pub struct NativeMarkdownPagedInput {
    target: InputTarget,
    selection: NativeMarkdownSelectionState,
    selection_ranges: RefCell<Vec<Range<usize>>>,
    marked: Cell<Option<NSRange>>,
    folds: RefCell<Vec<MarkdownFoldRegion>>,
    fold_revision: Cell<u64>,
    closed: Cell<bool>,
    busy: Cell<bool>,
}

struct Mutation<'a>(&'a Cell<bool>);
impl Drop for Mutation<'_> {
    fn drop(&mut self) {
        self.0.set(false); // 同步栈没有后继owner可替换此槽；错误和展开都只归还本次占用。
    }
}

struct PreparedFolds {
    folds: Vec<MarkdownFoldRegion>,
    length: usize,
    selection: MarkdownTextSelection,
}

struct FoldObserver<'a> {
    owner: &'a NativeMarkdownPagedInput,
    before: NativeMarkdownPagedInputState,
    mapper: MarkdownFoldAnchorMapping,
}

impl MarkdownTextChangeObserver for FoldObserver<'_> {
    type Output = PreparedFolds;
    type Error = NativeMarkdownPagedInputObserverError;

    fn changed(&mut self, change: MarkdownTextChange) -> std::result::Result<(), Self::Error> {
        if !self.owner.metadata_matches(self.before) {
            return Err(Self::Error::Revoked); // 这里只核纯metadata；Core可变借用内不调用current或Native。
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
    ) -> std::result::Result<Self::Output, Self::Error> {
        if !self.owner.metadata_matches(self.before) {
            return Err(Self::Error::Revoked);
        }
        let ranges = self
            .mapper
            .finish(candidate.utf16_len(), Some(selection_after.head_utf16))?;
        let mut folds = Vec::with_capacity(ranges.len());
        for hidden in ranges {
            // 原mapper最多256项；header来自真实候选树，旧行号不能进入新generation。
            folds.push(MarkdownFoldRegion {
                header_line: candidate.line_for_utf16(hidden.start)?,
                hidden_utf16_range: hidden,
            });
        }
        Ok(PreparedFolds {
            folds,
            length: candidate.utf16_len(),
            selection: selection_after,
        })
    }
}

impl NativeMarkdownPagedInput {
    /// 借原模型初态建立唯一选择事实；调用方须在替换同Rc内的Doc前撤销原epoch及本owner。
    ///
    /// # Errors
    /// 文档不存在、正在借用或初始选择无效时拒绝；不创建Native图或第二canonical owner。
    pub fn new(document: Weak<RefCell<MarkdownDocumentBuffer>>) -> Result<Rc<Self>> {
        Self::with_folds(document, Vec::new())
    }

    fn with_folds(
        document: Weak<RefCell<MarkdownDocumentBuffer>>,
        folds: Vec<MarkdownFoldRegion>,
    ) -> Result<Rc<Self>> {
        let original = document
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let (generation, selection) = {
            let original = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            let selection = original.selection();
            original.byte_offset_for_utf16(selection.anchor_utf16)?;
            original.byte_offset_for_utf16(selection.head_utf16)?;
            MarkdownFoldAnchorMapping::new(original.utf16_len(), &folds, false)?;
            (original.generation(), selection)
        };
        drop(original); // 初态读取的临时强claim在返回owner之前结束；长期字段只有Weak。
        Ok(Rc::new(Self {
            target: InputTarget::Body(document),
            selection: NativeMarkdownSelectionState::new(generation, selection),
            selection_ranges: RefCell::new(Vec::new()),
            marked: Cell::new(None),
            folds: RefCell::new(folds),
            fold_revision: Cell::new(0),
            closed: Cell::new(false),
            busy: Cell::new(false),
        }))
    }

    /// 返回真实canonical元数据；外部绕过本owner修改选择/代次后拒绝，不自动采新状态续权。
    ///
    /// # Errors
    /// 封闭、Doc释放、借用冲突或完整选择事实失配时拒绝。
    pub fn state(&self) -> Result<NativeMarkdownPagedInputState> {
        if self.closed.get() {
            return Err(NativeMarkdownPagedInputError::Closed);
        }
        if let Some(state) = self.cell_state()? {
            return Ok(state);
        }
        let document = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = document
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let generation = document.generation();
        let selection = document.selection();
        if self.selection.known_selection(generation, selection) != Some(selection) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let ranges = self
            .selection_ranges
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let primary_range = ranges.first().map_or_else(
            || ordered_selection_range(selection),
            |range| NSRange::new(range.start, range.len()),
        );
        Ok(NativeMarkdownPagedInputState {
            generation,
            selection,
            selection_is_directed: ranges.is_empty(),
            selection_range_count: ranges.len().max(1),
            primary_range,
            marked: self.marked.get(),
            utf16_len: document.utf16_len(),
            selection_serial: self.selection.serial(),
            fold_revision: self.fold_revision.get(),
        })
    }

    /// 新视图只借同一输入owner的原Doc身份，不能另传同内容文档冒充原canonical。
    pub(super) fn document_identity(&self) -> Weak<RefCell<MarkdownDocumentBuffer>> {
        self.target.body_document()
    }

    /// 固定原Body/Cell位置域；不导出Doc给局部范围补签，Scene另保原Source capture权限。
    pub fn target_domain(&self) -> Result<NativeMarkdownPagedTargetDomain> {
        self.state()?;
        self.target.domain()
    }

    /// 首个Native输入读取前固定原epoch；current必须验证既有前台与该epoch，不能补签较新票。
    ///
    /// # Errors
    /// 已占用或原权限/完整状态不再成立时拒绝。current可能重入，所有调用均在借用之外。
    pub fn capture(
        self: &Rc<Self>,
        epoch: u64,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputPermit> {
        if self.busy.get() {
            return Err(NativeMarkdownPagedInputError::Busy);
        }
        if !current(epoch) {
            return Err(NativeMarkdownPagedInputError::Revoked);
        }
        let before = self.state()?;
        let cell = self.target.cell_permit()?;
        self.check_original(epoch, before, current)?;
        if self.busy.get() {
            return Err(NativeMarkdownPagedInputError::Busy);
        }
        Ok(NativeMarkdownPagedInputPermit {
            owner: Rc::downgrade(self),
            epoch,
            before,
            consumed: Cell::new(false),
            cell,
        })
    }

    fn metadata_matches(&self, state: NativeMarkdownPagedInputState) -> bool {
        !self.closed.get()
            && self.selection.serial() == state.selection_serial
            && self
                .selection
                .known_selection(state.generation, state.selection)
                == Some(state.selection)
            && self.marked.get() == state.marked
            && self.fold_revision.get() == state.fold_revision
            && self.selection_ranges.try_borrow().is_ok_and(|ranges| {
                ranges.len().max(1) == state.selection_range_count
                    && ranges.is_empty() == state.selection_is_directed
                    && ranges.first().map_or_else(
                        || ordered_selection_range(state.selection),
                        |range| NSRange::new(range.start, range.len()),
                    ) == state.primary_range
            })
    }

    fn check_original(
        &self,
        epoch: u64,
        state: NativeMarkdownPagedInputState,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<()> {
        let source_health = || -> Result<()> {
            if self.is_cell() {
                self.cell_state()?; // 已提交receipt不再持编辑胶囊，仍沿同会话核真实Source首错。
            } else {
                let original = self
                    .document_identity()
                    .upgrade()
                    .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
                let document = original
                    .try_borrow()
                    .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
                for observer in document.backing_observers() {
                    // 含已退休段的原票；publish后的真实Drop失败必须由原Commit报告为已提交后果。
                    if let Some(errno) = observer.first_errno() {
                        return Err(
                            MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
                        );
                    }
                }
            }
            Ok(())
        };
        source_health()?;
        let permitted = current(epoch);
        source_health()?; // 可重入回调或最后旧Snapshot释放所发现的Source首错优先于撤权。
        if !permitted {
            return Err(NativeMarkdownPagedInputError::Revoked);
        }
        if self.state()? != state {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        // state()的Doc借用已结束；末次可重入检查后再纯核原完整状态，不能只核前一次结果。
        let permitted = current(epoch);
        source_health()?;
        if !permitted {
            return Err(NativeMarkdownPagedInputError::Revoked);
        }
        if self.state()? != state {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        Ok(())
    }

    /// 原输入票跨过真实外部操作后仍须核同一owner与原状态，不重捕后继输入续权。
    pub(super) fn check_permit(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<()> {
        if !std::ptr::eq(permit.owner.as_ptr(), self) {
            return Err(NativeMarkdownPagedInputError::Revoked);
        }
        if let Some(cell) = &permit.cell {
            cell.facts()?; // 同owner的原胶囊Source首错先于前台撤销，不读取外来permit的另一个Source。
        }
        self.check_original(permit.epoch, permit.before, current)
    }

    fn begin<'a>(
        &'a self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Mutation<'a>> {
        if self.busy.get() {
            return Err(NativeMarkdownPagedInputError::Busy);
        }
        if permit.consumed.get() {
            return Err(NativeMarkdownPagedInputError::Consumed);
        }
        self.check_permit(permit, current)?;
        if self.busy.get() {
            return Err(NativeMarkdownPagedInputError::Busy);
        }
        if permit.consumed.replace(true) {
            // current可能同步重入并先消费同票；这种拒绝不能伪装成另一在途事务。
            return Err(NativeMarkdownPagedInputError::Consumed);
        }
        self.selection
            .serial()
            .checked_add(1)
            .ok_or(NativeMarkdownPagedInputError::CounterExhausted)?;
        self.fold_revision
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownPagedInputError::CounterExhausted)?;
        self.busy.set(true);
        Ok(Mutation(&self.busy))
    }

    fn observer(&self, before: NativeMarkdownPagedInputState) -> Result<FoldObserver<'_>> {
        let folds = self
            .folds
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mapper = MarkdownFoldAnchorMapping::new(before.utf16_len, &folds, false)?;
        Ok(FoldObserver {
            owner: self,
            before,
            mapper,
        })
    }

    /// 直接处理同一次真实新增输入；范围为全局UTF16，NSNotFound优先替换原marked再取完整选择。
    ///
    /// # Errors
    /// 所有Err均先于Core发布；成功后的原票失效由返回Commit的check_current独立报告。
    pub fn insert_text(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &str,
        replacement: NSRange,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        let intent = self.cell_intent(permit, MarkdownCellDraftInputKind::Typing)?;
        self.replace(permit, text, replacement, None, intent, current)
    }

    /// selected相对本次候选，marked只在同一Core事务成功后发布；失败保留旧候选和历史。
    ///
    /// # Errors
    /// 非完整scalar范围、相对选择越界、资源失败或原票撤销均拒绝，不裁短输入。
    pub fn set_marked_text(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &str,
        selected: NSRange,
        replacement: NSRange,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        let intent = self.cell_intent(permit, MarkdownCellDraftInputKind::Marked)?;
        self.replace(permit, text, replacement, Some(selected), intent, current)
    }

    /// 原Native对象只借本回调；复用既有输入转换上限，绝不读取原文或构造隐藏NSTextView。
    ///
    /// # Errors
    /// 未支持的输入类型、转换期间撤权或后续Core预检失败均保留原状态。
    pub fn insert_native(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &AnyObject,
        replacement: NSRange,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        self.insert_native_kind(
            permit,
            text,
            replacement,
            MarkdownCellDraftInputKind::Typing,
            current,
        )
    }

    /// 显式隔离的新增载荷；不声称尚未测量的paste分组等同旧field，Body仍走同一事务实现。
    ///
    /// # Errors
    /// 与insert_native相同，原输入票只消费一次，失败不发布任何字符。
    pub fn insert_native_isolated(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &AnyObject,
        replacement: NSRange,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        self.insert_native_kind(
            permit,
            text,
            replacement,
            MarkdownCellDraftInputKind::Isolated,
            current,
        )
    }

    fn insert_native_kind(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &AnyObject,
        replacement: NSRange,
        kind: MarkdownCellDraftInputKind,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        let intent = self.cell_intent(permit, kind)?;
        let text = self.copy_permit_text(permit, text, current)?;
        self.replace(permit, &text, replacement, None, intent, current)
    }

    /// 与insert_native相同的原票覆盖完整Native候选读取和相对选择校验。
    ///
    /// # Errors
    /// 不截断候选、不忽略无效属性字符串类型，也不把读取失败当成空marked。
    pub fn set_marked_native(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &AnyObject,
        selected: NSRange,
        replacement: NSRange,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        let intent = self.cell_intent(permit, MarkdownCellDraftInputKind::Marked)?;
        let text = self.copy_permit_text(permit, text, current)?;
        self.replace(permit, &text, replacement, Some(selected), intent, current)
    }

    fn copy_permit_text(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        text: &AnyObject,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<String> {
        self.copy_permit_text_with_limit(permit, text, current, &|bytes| {
            bytes <= NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
        })
    }

    /// 延后普通输入复用同一typed首错和Native转换；额度只限制本次新增副本，不读取正文。
    pub(in crate::native_surface) fn copy_permit_text_with_limit(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        text: &AnyObject,
        current: &(impl Fn(u64) -> bool + ?Sized),
        reserve_bytes: &impl Fn(usize) -> bool,
    ) -> Result<String> {
        let first = RefCell::new(None);
        let converted = super::text_storage::copy_native_markdown_input_text_with_limit(
            text,
            &|| {
                match self.check_permit(permit, current) {
                    Ok(()) => true,
                    Err(error) => {
                        // 仅栈内固定首错槽；借用在返回bool之前结束，不跨后续Native载荷getter或current。
                        first.borrow_mut().get_or_insert(error);
                        false
                    }
                }
            },
            reserve_bytes,
        );
        if let Some(error) = first.into_inner() {
            return Err(error); // 真实Source/原票失败优先，不能由转换层的String错误覆盖typed因果。
        }
        converted.map_err(NativeMarkdownPagedInputError::NativeInput)
    }

    fn cancel(
        &self,
        document: &RefCell<MarkdownDocumentBuffer>,
        token: &MarkdownPreparedTextInput,
        cause: NativeMarkdownPagedInputError,
    ) -> NativeMarkdownPagedInputError {
        let cleanup = document
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputCancellationError::Borrowed)
            .and_then(|mut document| {
                document
                    .cancel_prepared_text_input(token)
                    .map_err(NativeMarkdownPagedInputCancellationError::Document)
            });
        match cleanup {
            Ok(()) => cause, // 清理成功不恢复原Source权限，原首错仍返回调用方。
            Err(cleanup) => NativeMarkdownPagedInputError::CancellationUnconfirmed {
                cause: Box::new(cause),
                cleanup,
            },
        }
    }

    fn replace(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &str,
        requested: NSRange,
        relative: Option<NSRange>,
        intent: Option<cell_mutations::CellInputIntents>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        if text.len() > NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES {
            return Err(NativeMarkdownPagedInputError::InputTooLarge);
        }
        self.check_permit(permit, current)?;
        let range = replacement_range(permit.before, requested)?;
        let units = text.encode_utf16().count();
        let end = range
            .start
            .checked_add(units)
            .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
        let after = match relative {
            None => MarkdownTextSelection::caret(end), // 与原生产输入相同，显式替换也发布本次末端单caret。
            Some(selected) => {
                let selected_end = selected
                    .location
                    .checked_add(selected.length)
                    .ok_or(NativeMarkdownPagedInputError::InvalidMarkedSelection)?;
                if selected_end > units
                    || !text_boundary(text, selected.location)
                    || !text_boundary(text, selected_end)
                {
                    return Err(NativeMarkdownPagedInputError::InvalidMarkedSelection);
                }
                MarkdownTextSelection {
                    anchor_utf16: range
                        .start
                        .checked_add(selected.location)
                        .ok_or(NativeMarkdownPagedInputError::InvalidRange)?,
                    head_utf16: range
                        .start
                        .checked_add(selected_end)
                        .ok_or(NativeMarkdownPagedInputError::InvalidRange)?,
                }
            }
        };
        let marked = (relative.is_some() && units != 0).then_some(NSRange::new(range.start, units));
        if let Some(intent) = intent {
            return self.replace_cell(
                permit,
                cell_mutations::CellReplacement {
                    text,
                    range,
                    selection: after,
                    marked,
                    intent: intent.into_intent(relative.is_some() && units == 0),
                },
                current,
            );
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        let (token, prepared) = {
            let mut document = original
                .try_borrow_mut()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if !self.matches_document(&document, permit.before) {
                return Err(NativeMarkdownPagedInputError::StateChanged);
            }
            // 只按原permit的真实marked翻译；新Input首个Start不继承同Doc旧client遗留的组标记。
            let composition = match (relative.is_some(), permit.before.marked) {
                (true, None) => Some(MarkdownTextInputComposition::Start),
                (true, Some(marked)) => Some(MarkdownTextInputComposition::Update {
                    marked_before: marked.location..marked.location + marked.length,
                }),
                (false, Some(marked)) => Some(MarkdownTextInputComposition::Commit {
                    marked_before: marked.location..marked.location + marked.length,
                }),
                (false, None) => None, // 普通输入不取得任何旧组合的合并权限。
            };
            if let Some(composition) = composition {
                // marked更新与确认沿同一个Core组合组；显式子范围是否可合并由原组真实范围决定。
                document.prepare_composition_text_input_observed(
                    range,
                    text,
                    permit.before.selection,
                    after,
                    composition,
                    observer,
                )?
            } else {
                document.prepare_text_input_observed(
                    range,
                    text,
                    permit.before.selection,
                    after,
                    observer,
                )?
            }
        };
        // prepare的Doc/fold借用已结束，原前台检查可同步重入；失权只能取消同nonce。
        if let Err(cause) = self.check_permit(permit, current) {
            return Err(self.cancel(&original, &token, cause));
        }
        let mut document = match original.try_borrow_mut() {
            Ok(document) => document,
            Err(_) => {
                return Err(self.cancel(
                    &original,
                    &token,
                    NativeMarkdownPagedInputError::Borrowed,
                ));
            }
        };
        let mut folds = match self.folds.try_borrow_mut() {
            Ok(folds) => folds,
            Err(_) => {
                drop(document);
                return Err(self.cancel(
                    &original,
                    &token,
                    NativeMarkdownPagedInputError::Borrowed,
                ));
            }
        };
        if !self.matches_document(&document, permit.before) || prepared.selection != after {
            drop(folds);
            drop(document);
            return Err(self.cancel(
                &original,
                &token,
                NativeMarkdownPagedInputError::StateChanged,
            ));
        }
        let edit = match document.publish_text_input(&token) {
            Ok(edit) => edit,
            Err(error) => {
                drop(folds);
                drop(document);
                return Err(self.cancel(&original, &token, error.into()));
            }
        };
        // 从publish到metadata完成没有Native、current或可失败IO；不允许提交后再用普通Err谎报未修改。
        let commit = self.publish(
            permit,
            document.generation(),
            prepared,
            marked,
            Some(edit),
            &mut folds,
        );
        drop(folds);
        drop(document);
        drop(original);
        drop(token);
        Ok(commit)
    }

    fn matches_document(
        &self,
        document: &MarkdownDocumentBuffer,
        state: NativeMarkdownPagedInputState,
    ) -> bool {
        self.metadata_matches(state)
            && document.generation() == state.generation
            && document.selection() == state.selection
            && document.utf16_len() == state.utf16_len
    }

    fn publish(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        generation: u64,
        prepared: PreparedFolds,
        marked: Option<NSRange>,
        edit: Option<MarkdownHistoryTextEdit>,
        folds: &mut Vec<MarkdownFoldRegion>,
    ) -> NativeMarkdownPagedInputCommit {
        let folds_changed = *folds != prepared.folds;
        *folds = prepared.folds; // 旧有限Vec在纯Rust发布内释放，不跨Native调用保留容量。
        self.fold_revision.set(self.fold_revision.get() + 1); // begin已预检checked_add且busy禁止第二发布。
        self.marked.set(marked);
        // 生产输入和历史发布一个明确after，旧多范围在同一不可重入栈内释放。
        drop(self.selection_ranges.replace(Vec::new()));
        let serial = self
            .selection
            .publish(generation, prepared.selection, true, None, None);
        let state = NativeMarkdownPagedInputState {
            generation,
            selection: prepared.selection,
            selection_is_directed: true,
            selection_range_count: 1,
            primary_range: ordered_selection_range(prepared.selection),
            marked,
            utf16_len: prepared.length,
            selection_serial: serial,
            fold_revision: self.fold_revision.get(),
        };
        NativeMarkdownPagedInputCommit {
            owner: Rc::downgrade(self),
            epoch: permit.epoch,
            state,
            invalidation: NativeMarkdownPagedInputInvalidation {
                replaced_utf16_range: edit.as_ref().map(|edit| edit.replaced_utf16_range.clone()),
                change_in_utf16_length: edit.as_ref().map_or(0, |edit| edit.change_in_utf16_length),
                characters_changed: generation != permit.before.generation,
                selection_changed: prepared.selection != permit.before.selection
                    || !permit.before.selection_is_directed,
                marked_changed: marked != permit.before.marked,
                folds_changed,
            },
        }
    }

    /// 解除标记只改变原owner的组合metadata；不创建候选、追加、日志或历史事务。
    ///
    /// # Errors
    /// 原票或Core完整选择失配时保留marked；只有真实无marked才返回Unchanged。
    pub fn unmark(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if permit.before.marked.is_none() {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        if permit.cell.is_some() {
            return self.unmark_cell(permit);
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) || document.has_prepared_text_input() {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        document.finish_text_input_composition()?;
        // Core只结束同一组合的历史归属；bytes/root/g/history及资源均不变，再发布真实无marked状态。
        self.marked.set(None);
        let side = self
            .selection
            .known_head_is_upstream(permit.before.generation, permit.before.selection);
        let goal = self
            .selection
            .known_vertical_goal(permit.before.generation, permit.before.selection);
        let selection_serial = self.selection.publish(
            permit.before.generation,
            permit.before.selection,
            true,
            side,
            goal,
        );
        let state = NativeMarkdownPagedInputState {
            marked: None,
            selection_serial,
            ..permit.before
        };
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            NativeMarkdownPagedInputCommit {
                owner: Rc::downgrade(self),
                epoch: permit.epoch,
                state,
                invalidation: NativeMarkdownPagedInputInvalidation {
                    replaced_utf16_range: None,
                    change_in_utf16_length: 0,
                    characters_changed: false,
                    selection_changed: false,
                    marked_changed: true,
                    folds_changed: false,
                },
            },
        ))
    }

    /// 直接复用Core已预检的observed历史事务，不把Undo伪装为一次新replacement。
    ///
    /// # Errors
    /// 有marked、原票撤销、真实日志/源IO或fold预检失败时保留当前正文、历史与组合状态。
    pub fn history(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        direction: NativeMarkdownPagedInputHistory,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        }
        if permit.cell.is_some() {
            return self.history_cell(permit, direction);
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let observer = self.observer(permit.before)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut folds = self
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let restored = match direction {
            NativeMarkdownPagedInputHistory::Undo => document.undo_text_edit_observed(observer)?,
            NativeMarkdownPagedInputHistory::Redo => document.redo_text_edit_observed(observer)?,
        };
        let Some((edit, prepared)) = restored else {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        };
        let commit = self.publish(
            permit,
            document.generation(),
            prepared,
            None,
            Some(edit),
            &mut folds,
        );
        drop(folds);
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(commit))
    }

    /// 鼠标、导航和全选只发布同票选择，不通过另一次文字事务制造历史或代次。
    ///
    /// # Errors
    /// 组合仍在进行、原票撤销、选择切入scalar或Core正准备事务时保留完整原状态。
    pub fn set_selection(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        self.publish_selection(permit, selection, None, current)
    }

    /// 原导航的方向、换行侧和目标列与同一选择serial一起发布；相同数值范围仍是新意图。
    /// 只改唯一选择槽，不改文字generation，也不创建Undo历史或第二套导航缓存。
    ///
    /// # Errors
    /// 与普通set_selection相同；坏目标列在Core选择改变前拒绝，原事实保持完整。
    pub fn set_selection_intent(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        head_upstream: Option<bool>,
        vertical_goal: Option<NativeMarkdownVerticalGoal>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        self.publish_selection(
            permit,
            selection,
            Some((head_upstream, vertical_goal, None)),
            current,
        )
    }

    /// 原视觉导航把真实Bidi侧与同一次选择serial发布；同offset换侧也撤销旧输入票。
    ///
    /// # Errors
    /// 与普通选择意图相同；不改文字generation、Undo或另建一个选择缓存。
    pub fn set_visual_selection_intent(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        head_upstream: bool,
        bidi_side: NativeMarkdownBidiCaretSide,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        self.publish_selection(
            permit,
            selection,
            Some((Some(head_upstream), None, Some(bidi_side))),
            current,
        )
    }

    /// 只返回同原选择槽的视觉侧；current重入撤销几何后不能复活回调前的侧。
    pub fn navigation_bidi_side(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Option<NativeMarkdownBidiCaretSide>> {
        self.check_permit(permit, current)?;
        let side = self
            .selection
            .known_bidi_side(permit.before.generation, permit.before.selection);
        self.check_permit(permit, current)?;
        if side
            != self
                .selection
                .known_bidi_side(permit.before.generation, permit.before.selection)
        {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        Ok(side)
    }

    /// 只读原permit对应的同一选择槽；多选未知方向不能从canonical历史memento恢复主head。
    ///
    /// # Errors
    /// 原输入、完整状态或前台失权时拒绝；纯事实读取不延长Doc、Native对象或Source生命周期。
    pub fn navigation_selection_facts(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<(Option<bool>, Option<NativeMarkdownVerticalGoal>)> {
        self.check_permit(permit, current)?;
        let facts = self.navigation_facts_for(permit.before);
        self.check_permit(permit, current)?;
        if facts != self.navigation_facts_for(permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        } // current可能同步清除了几何而未改canonical数值，旧事实也不能在返回时复活。
        Ok(facts)
    }

    fn navigation_facts_for(
        &self,
        state: NativeMarkdownPagedInputState,
    ) -> (Option<bool>, Option<NativeMarkdownVerticalGoal>) {
        if state.selection_is_directed {
            (
                self.selection
                    .known_head_is_upstream(state.generation, state.selection),
                self.selection
                    .known_vertical_goal(state.generation, state.selection),
            )
        } else {
            (None, None) // 有序主范围不是有方向的选择，不能猜一个上游/下游事实。
        }
    }

    /// 原Source/样式/排版坐标变化时撤除几何事实；逻辑anchor/head和历史保持原样。
    pub fn clear_navigation_geometry(&self) {
        self.selection.clear_vertical_goal();
    }

    fn publish_selection(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        intent: Option<(
            Option<bool>,
            Option<NativeMarkdownVerticalGoal>,
            Option<NativeMarkdownBidiCaretSide>,
        )>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        let _mutation = self.begin(permit, current)?;
        self.check_permit(permit, current)?;
        if permit.before.marked.is_some() {
            return Err(NativeMarkdownPagedInputError::CompositionActive);
        }
        if permit.cell.is_some() {
            return self.selection_cell(permit, selection, intent);
        }
        let original = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let mut document = original
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        if !self.matches_document(&document, permit.before) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        let invalid_geometry = intent.is_some_and(|(head, goal, side)| {
            goal.is_some_and(|goal| !goal.is_finite() || head.is_none())
                || side.is_some_and(|side| side.level > 126 || head.is_none())
        });
        if invalid_geometry {
            // 首错仍先归原Source；不能让坏目标列掩盖已经发生的Backing故障，也不改变选择。
            document.byte_offset_for_utf16(permit.before.selection.anchor_utf16)?;
            return Err(NativeMarkdownPagedInputError::InvalidRange);
        }
        document.set_selection(selection)?;
        if intent.is_none()
            && selection == permit.before.selection
            && permit.before.selection_is_directed
        {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        // set_selection已核源健康、idle和两端scalar；后面只发布有限事实，没有Native/current/IO。
        let (head_upstream, goal, side) = intent.unwrap_or((None, None, None));
        let serial = self.selection.publish_with_bidi_side(
            permit.before.generation,
            selection,
            true,
            head_upstream,
            goal,
            side,
        );
        drop(self.selection_ranges.replace(Vec::new()));
        let state = NativeMarkdownPagedInputState {
            selection,
            selection_is_directed: true,
            selection_range_count: 1,
            primary_range: ordered_selection_range(selection),
            selection_serial: serial,
            ..permit.before
        };
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(
            NativeMarkdownPagedInputCommit {
                owner: Rc::downgrade(self),
                epoch: permit.epoch,
                state,
                invalidation: NativeMarkdownPagedInputInvalidation {
                    replaced_utf16_range: None,
                    change_in_utf16_length: 0,
                    characters_changed: false,
                    selection_changed: true,
                    marked_changed: false,
                    folds_changed: false,
                },
            },
        ))
    }

    /// 原close预检可读此值；事务占用中不能接受会释放同一文档的关闭。
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy.get()
    }

    /// 封闭只释放有限metadata，原文与映射始终归composition root；旧Permit/Commit立即失权。
    ///
    /// # Errors
    /// 当前同步输入尚未结束时拒绝，不清候选或把仍活输入伪装成已关闭。
    pub fn close(&self) -> Result<()> {
        if self.closed.get() {
            return Ok(());
        } // 重复清理不推进serial、不重建容量，也不影响后继owner。
        if self.busy.get() {
            return Err(NativeMarkdownPagedInputError::Busy);
        }
        self.closed.set(true);
        self.marked.set(None);
        if self.selection.serial() != u64::MAX {
            self.selection.seal();
        }
        // serial已耗尽时closed仍永久拒绝；不回绕计数或调用内部expect制造teardown panic。
        let previous = self.folds.replace(Vec::new());
        drop(previous);
        drop(self.selection_ranges.replace(Vec::new()));
        Ok(())
    }
}

fn replacement_range(
    state: NativeMarkdownPagedInputState,
    requested: NSRange,
) -> Result<Range<usize>> {
    let range = if requested.location != NSNotFound as usize {
        requested
    } else if let Some(marked) = state.marked {
        marked
    } else {
        if !state.selection_is_directed {
            // 原产品未指定输入不从实际Multiple猜主替换范围；保留全部选择和原文等待明确动作。
            return Err(NativeMarkdownPagedInputError::MultipleSelectionInput);
        }
        state.primary_range
    };
    let end = range
        .location
        .checked_add(range.length)
        .ok_or(NativeMarkdownPagedInputError::InvalidRange)?;
    if end > state.utf16_len {
        return Err(NativeMarkdownPagedInputError::InvalidRange);
    }
    Ok(range.location..end)
}

fn ordered_selection_range(selection: MarkdownTextSelection) -> NSRange {
    NSRange::new(
        selection.anchor_utf16.min(selection.head_utf16),
        selection.anchor_utf16.abs_diff(selection.head_utf16),
    )
}

fn text_boundary(text: &str, at: usize) -> bool {
    let mut units = 0;
    for scalar in text.chars() {
        if units == at {
            return true;
        }
        units += scalar.len_utf16();
        if units > at {
            return false;
        } // 低代理位置不能变成另一个看似有效的候选选择。
    }
    units == at
}

#[cfg(test)]
mod tests;

mod replacements;
mod selection_ranges;
pub use selection_ranges::NATIVE_MARKDOWN_PAGED_SELECTION_RANGE_LIMIT;
