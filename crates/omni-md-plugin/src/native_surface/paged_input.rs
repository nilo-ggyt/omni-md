//! The paged input transaction boundary weakly borrows its original target; it owns neither TextKit storage nor visible geometry.

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
mod folding;
mod formatting;
#[cfg(feature = "native-input-trace")]
mod input_trace;
mod source_commands;
mod target;
pub use cell::NativeMarkdownPagedCellSession;
pub use cell_commit::{
    NativeMarkdownPagedCellCommit, NativeMarkdownPagedCellCommitError,
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
    #[error("无法访问原单元格来源：{0:?}")]
    CellSource(omni_markdown_core::markdown_document_buffer::MarkdownTableRequestError),
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
/// Target history selection and bounded interactive facts; multiple ranges do not authorize a directed primary anchor/head.
pub struct NativeMarkdownPagedInputState {
    pub generation: u64,
    /// The Core memento also represents interactive anchor/head only when selection_is_directed is true.
    pub selection: MarkdownTextSelection,
    pub selection_is_directed: bool,
    pub selection_range_count: usize,
    /// The ordered NSTextInputClient primary range cannot recover the unknown direction of a multiple selection.
    pub primary_range: NSRange,
    pub marked: Option<NSRange>,
    pub utf16_len: usize,
    pub selection_serial: u64,
    pub fold_revision: u64,
}

impl NativeMarkdownPagedInputState {
    /// Multiple ranges retain their actual set and primary range without inventing a primary head for navigation or Find.
    #[must_use]
    pub fn directed_selection(self) -> Option<MarkdownTextSelection> {
        self.selection_is_directed.then_some(self.selection)
    }
}

/// One original callback permit: it is not Clone, and the first attempt consumes it even if a different payload could succeed.
pub struct NativeMarkdownPagedInputPermit {
    owner: Weak<NativeMarkdownPagedInput>,
    epoch: u64,
    before: NativeMarkdownPagedInputState,
    consumed: Cell<bool>,
    cell: Option<cell::CellPermit>,
}

#[derive(Clone, Debug)]
/// Describes committed source changes; subsequent layout computes visible damage under its own original authority.
pub struct NativeMarkdownPagedInputInvalidation {
    pub replaced_utf16_range: Option<Range<usize>>,
    pub change_in_utf16_length: isize,
    pub characters_changed: bool,
    pub selection_changed: bool,
    pub marked_changed: bool,
    pub folds_changed: bool,
}

/// A successful return means Core has published; later revocation can stop presentation but cannot undo that fact.
pub struct NativeMarkdownPagedInputCommit {
    owner: Weak<NativeMarkdownPagedInput>,
    epoch: u64,
    pub state: NativeMarkdownPagedInputState,
    pub invalidation: NativeMarkdownPagedInputInvalidation,
}

impl NativeMarkdownPagedInputCommit {
    /// Validate this committed effect before a View emits reentrant geometry or display notifications; never recapture a successor.
    ///
    /// # Errors
    /// Reject a changed foreground epoch, owner, or complete committed state; rejection does not roll back published text.
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
    /// Only genuine empty history or an already unmarked state is unchanged; errors must remain errors.
    Unchanged(NativeMarkdownPagedInputState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMarkdownPagedInputHistory {
    Undo,
    Redo,
}

/// The composition root owns the Rc; this owner holds only a weak document identity and bounded derived metadata, not mappings.
pub struct NativeMarkdownPagedInput {
    target: InputTarget,
    selection: NativeMarkdownSelectionState,
    selection_ranges: RefCell<Vec<Range<usize>>>,
    marked: Cell<Option<NSRange>>,
    folds: RefCell<Vec<MarkdownFoldRegion>>,
    fold_revision: Cell<u64>,
    closed: Cell<bool>,
    busy: Cell<bool>,
    // Allocate the bounded recorder only after explicit arming; it owns no document, Native object, or asynchronous back-reference.
    #[cfg(feature = "native-input-trace")]
    input_trace: RefCell<Option<crate::native_input_trace::SharedNativeInputTrace>>,
}

struct Mutation<'a>(&'a Cell<bool>);
impl Drop for Mutation<'_> {
    fn drop(&mut self) {
        self.0.set(false); // Only this synchronous stack owns the occupied slot; errors and unwinding release that same admission.
    }
}

struct PreparedFolds {
    // The observer transfers bounded derived facts only; the candidate tree and
    // original text/mappings remain with the Core transaction.
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
            return Err(Self::Error::Revoked); // Inspect pure metadata only: Core holds a mutable borrow, so current and Native must not run here.
        }
        // Consume actual changes in old coordinates; an envelope would erase folds
        // between disjoint replacements or the two markers of a surround format.
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
            // The mapper has at most 256 anchors; derive headers from the actual candidate tree rather than reusing old-generation line numbers.
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
    /// Initialize the sole selection authority from the original model; revoke its epoch and this owner before replacing the document inside that Rc.
    ///
    /// # Errors
    /// Reject a released or borrowed document or invalid initial selection; create neither a Native graph nor another canonical owner.
    pub fn new(document: Weak<RefCell<MarkdownDocumentBuffer>>) -> Result<Rc<Self>> {
        Self::with_folds(document, Vec::new())
    }

    fn with_folds(
        document: Weak<RefCell<MarkdownDocumentBuffer>>,
        folds: Vec<MarkdownFoldRegion>,
    ) -> Result<Rc<Self>> {
        // Validate endpoints against the existing canonical tree without taking a
        // text snapshot or creating an independent Source owner.
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
        drop(original); // End the temporary strong claim before returning the input owner; the retained identity is weak.
        Ok(Rc::new(Self {
            target: InputTarget::Body(document),
            selection: NativeMarkdownSelectionState::new(generation, selection),
            selection_ranges: RefCell::new(Vec::new()),
            marked: Cell::new(None),
            folds: RefCell::new(folds),
            fold_revision: Cell::new(0),
            closed: Cell::new(false),
            busy: Cell::new(false),
            #[cfg(feature = "native-input-trace")]
            input_trace: RefCell::new(None),
        }))
    }

    /// Return canonical metadata only while it agrees with this owner; out-of-band selection/generation changes revoke it rather than renew it.
    ///
    /// # Errors
    /// Reject closure, document release, a conflicting borrow, or disagreement in complete selection facts.
    pub fn state(&self) -> Result<NativeMarkdownPagedInputState> {
        if self.closed.get() {
            return Err(NativeMarkdownPagedInputError::Closed);
        }
        // Cell serials/UTF-16 offsets belong to its draft, never the canonical
        // document. A local mismatch cannot be repaired from Body metadata.
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

    /// Views borrow this input owner's original document identity; another document with equal bytes is not the canonical target.
    pub(super) fn document_identity(&self) -> Weak<RefCell<MarkdownDocumentBuffer>> {
        self.target.body_document()
    }

    /// Capture the original Body/Cell coordinate domain; Scene still checks its original Source capture independently.
    pub fn target_domain(&self) -> Result<NativeMarkdownPagedTargetDomain> {
        self.state()?;
        self.target.domain()
    }

    /// Fix the original epoch before the first Native payload read; current must validate that foreground epoch, not authorize a newer one.
    ///
    /// # Errors
    /// Reject busy or invalid original authority/state. Every current call is outside borrows because it can reenter.
    pub fn capture(
        self: &Rc<Self>,
        epoch: u64,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputPermit> {
        // current may reenter and complete an action. Check busy on both sides
        // while retaining the same before-state and original Cell capsule.
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

    // This runs inside Core's mutable document borrow: inspect bounded metadata
    // only, with no Source read, Native operation, or external callback.
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
                self.cell_state()?; // A committed receipt no longer holds the edit capsule, but the same session still checks the original Source failure.
            } else {
                let original = self
                    .document_identity()
                    .upgrade()
                    .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
                let document = original
                    .try_borrow()
                    .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
                for observer in document.backing_observers() {
                    // Include retired backing observers: a real Drop failure after publication remains an effect of this original commit.
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
        source_health()?; // A Source failure discovered by reentry or the last old snapshot release takes precedence over revocation.
        if !permitted {
            return Err(NativeMarkdownPagedInputError::Revoked);
        }
        if self.state()? != state {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        // state() has released its document borrow. Recheck complete original facts after the last reentrant callback.
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

    /// After an external operation, validate the same owner and original state without recapturing successor input.
    pub(super) fn check_permit(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<()> {
        if !std::ptr::eq(permit.owner.as_ptr(), self) {
            return Err(NativeMarkdownPagedInputError::Revoked);
        }
        if let Some(cell) = &permit.cell {
            cell.facts()?; // The original capsule's Source failure precedes foreground revocation; never inspect a foreign permit's Source.
        }
        self.check_original(permit.epoch, permit.before, current)
    }

    fn begin<'a>(
        &'a self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<Mutation<'a>> {
        // Reject occupied/consumed admission before reentry, then recheck both:
        // current itself may consume the same permit or start another action.
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
            // current may have reentered and consumed this permit; distinguish that from another busy transaction.
            return Err(NativeMarkdownPagedInputError::Consumed);
        }
        // Preflight counter capacity before any Core publication. Metadata after
        // a successful canonical edit must not fail partway through publication.
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
        // Copy the bounded projection into its mapper; return the fold borrow
        // before any subsequent reentrant current/Native call.
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

    /// Apply this newly supplied text in target UTF-16 coordinates; NSNotFound prefers the original marked range over the full selection.
    ///
    /// # Errors
    /// All errors precede Core publication; the returned commit separately reports revocation of an already published effect.
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

    /// Use the bounded payload converted when Paste arrived and its isolated intent without allocating another NSString.
    pub(in crate::native_surface) fn insert_text_isolated(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        text: &str,
        replacement: NSRange,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputCommit> {
        let _mutation = self.begin(permit, current)?;
        let intent = self.cell_intent(permit, MarkdownCellDraftInputKind::Isolated)?;
        self.replace(permit, text, replacement, None, intent, current)
    }

    /// selected is relative to this candidate; publish marked state only with its successful Core transaction, preserving the old candidate/history on failure.
    ///
    /// # Errors
    /// Reject scalar splits, out-of-range relative selection, resource failure, or revocation; never truncate input.
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

    /// Borrow the Native payload only for this callback and use the existing conversion limit; read no document text or hidden NSTextView.
    ///
    /// # Errors
    /// Unsupported payload types, revocation during conversion, and Core preflight errors preserve the original state.
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

    /// An explicitly isolated payload uses the same Body transaction; this does not claim unmeasured equivalence with the former field's Paste grouping.
    ///
    /// # Errors
    /// As with insert_native, consume the original permit once and publish no characters on failure.
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

    /// The same original permit covers the complete Native candidate read and relative-selection validation.
    ///
    /// # Errors
    /// Do not truncate a candidate, ignore an invalid attributed-string type, or reinterpret a failed read as empty marked text.
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

    /// Deferred ordinary input shares the typed first-error conversion; the limit bounds only this newly supplied copy, not document reads.
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
                        // Keep one stack-local first-error slot; release its borrow before returning to a Native payload getter or current.
                        first.borrow_mut().get_or_insert(error);
                        false
                    }
                }
            },
            reserve_bytes,
        );
        if let Some(error) = first.into_inner() {
            return Err(error); // Preserve the typed Source/permit cause instead of replacing it with a conversion-layer String error.
        }
        converted.map_err(NativeMarkdownPagedInputError::NativeInput)
    }

    fn cancel(
        &self,
        document: &RefCell<MarkdownDocumentBuffer>,
        token: &MarkdownPreparedTextInput,
        cause: NativeMarkdownPagedInputError,
    ) -> NativeMarkdownPagedInputError {
        // Cancel only this original prepare token. Preserve the primary cause
        // together with any inability to confirm cancellation.
        let cleanup = document
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputCancellationError::Borrowed)
            .and_then(|mut document| {
                document
                    .cancel_prepared_text_input(token)
                    .map_err(NativeMarkdownPagedInputCancellationError::Document)
            });
        match cleanup {
            Ok(()) => cause, // Successful cancellation does not restore Source authority; return the original cause.
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
        // Bound only the newly supplied UTF-8 payload. Existing text stays in the
        // canonical tree and is never materialized by this replacement adapter.
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
            None => MarkdownTextSelection::caret(end), // Explicit replacement also publishes a single caret at the inserted end, as the production input path does.
            Some(selected) => {
                // Candidate-relative selection uses UTF-16. Verify complete scalar
                // endpoints before translating it into the target coordinate domain.
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
            // Consume the captured Cell intent/capsule, including empty marked
            // input; Body folds/history have no authority in this local domain.
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
            // Translate only this permit's marked state; a new input owner's first Start cannot inherit another client's old group.
            let composition = match (relative.is_some(), permit.before.marked) {
                (true, None) => Some(MarkdownTextInputComposition::Start),
                (true, Some(marked)) => Some(MarkdownTextInputComposition::Update {
                    marked_before: marked.location..marked.location + marked.length,
                }),
                (false, Some(marked)) => Some(MarkdownTextInputComposition::Commit {
                    marked_before: marked.location..marked.location + marked.length,
                }),
                (false, None) => None, // Ordinary input receives no authority to merge with an old composition.
            };
            if let Some(composition) = composition {
                // Marked updates and confirmation use the same Core group; its actual range decides whether an explicit subrange can merge.
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
        // Preparation has released document/fold borrows. Foreground reentry may revoke this exact nonce, which must then be cancelled.
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
        // Preparation did not publish. Metadata drift cancels this nonce while
        // preserving the original canonical root and history.
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
        // From publication through metadata update there is no Native call, current, or fallible I/O; a committed edit cannot become an ordinary precommit error.
        let commit = self.publish(permit, &document, prepared, marked, Some(edit), &mut folds);
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
        document: &MarkdownDocumentBuffer,
        prepared: PreparedFolds,
        marked: Option<NSRange>,
        edit: Option<MarkdownHistoryTextEdit>,
        folds: &mut Vec<MarkdownFoldRegion>,
    ) -> NativeMarkdownPagedInputCommit {
        let generation = document.generation();
        let folds_changed = *folds != prepared.folds;
        // The system IME may insert empty text during activation/resignation. Core has still consumed the prepare token and checked Source health.
        // An empty SelectionOnly result must not advance selection/fold revisions and invalidate a just-captured first frame or mode ticket.
        // Compare actual effects: selection deletion, unmarking, fold remapping, and collapsing multiple ranges must still publish.
        let unchanged = generation == permit.before.generation
            && prepared.length == permit.before.utf16_len
            && prepared.selection == permit.before.selection
            && marked == permit.before.marked
            && !folds_changed
            && permit.before.selection_is_directed
            && permit.before.selection_range_count == 1;
        let state = if unchanged {
            permit.before // Preserve complete original Input, including navigation side, selection serial, and fold revision; issue no replacement authority.
        } else {
            *folds = prepared.folds; // Release the previous bounded Vec inside pure Rust publication, without carrying its capacity across Native calls.
            self.fold_revision.set(self.fold_revision.get() + 1); // begin preflighted checked_add, and busy excludes another publication.
            self.marked.set(marked);
            // Input/history publish one definite after-selection; release the old multiple ranges on this nonreentrant stack.
            drop(self.selection_ranges.replace(Vec::new()));
            let serial = self
                .selection
                .publish(generation, prepared.selection, true, None, None);
            NativeMarkdownPagedInputState {
                generation,
                selection: prepared.selection,
                selection_is_directed: true,
                selection_range_count: 1,
                primary_range: ordered_selection_range(prepared.selection),
                marked,
                utf16_len: prepared.length,
                selection_serial: serial,
                fold_revision: self.fold_revision.get(),
            }
        };
        let commit = NativeMarkdownPagedInputCommit {
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
        };
        #[cfg(feature = "native-input-trace")]
        self.record_input_trace_commit(&commit, document); // Observe scalar facts without Native/current; observation cannot change the published transaction.
        commit
    }

    /// Unmarking changes this owner's composition metadata without creating a candidate, append, journal, or history transaction.
    ///
    /// # Errors
    /// Keep marked text when the permit or complete Core selection disagrees; only an already unmarked state is Unchanged.
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
        // Core ends this composition's history grouping without changing bytes, root, generation, history entries, or resources; then publish unmarked facts.
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
        let commit = NativeMarkdownPagedInputCommit {
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
        };
        #[cfg(feature = "native-input-trace")]
        self.record_input_trace_commit(&commit, &document); // The real unmark completes the original composition even without byte changes.
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(commit))
    }

    /// Menus inspect the active client's actual history; Cell does not borrow Body undo availability, and composition blocks history actions.
    pub(in crate::native_surface) fn can_perform_history(
        &self,
        direction: NativeMarkdownPagedInputHistory,
    ) -> Result<bool> {
        if self.state()?.marked.is_some() {
            return Ok(false);
        }
        if let Some(facts) = self.cell_history_facts()? {
            return Ok(match direction {
                NativeMarkdownPagedInputHistory::Undo => facts.can_undo,
                NativeMarkdownPagedInputHistory::Redo => facts.can_redo,
            }); // Read draft history under the original Cell token without committing it or creating menu-only history.
        }
        let owner = self
            .document_identity()
            .upgrade()
            .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
        let document = owner
            .try_borrow()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        Ok(match direction {
            NativeMarkdownPagedInputHistory::Undo => document.can_undo(),
            NativeMarkdownPagedInputHistory::Redo => document.can_redo(),
        }) // Inspect actual stacks without reading text or changing selection, grouping, or disk state.
    }

    /// Reuse Core's preflighted observed history transaction; Undo is not a fresh replacement.
    ///
    /// # Errors
    /// Marked input, revocation, journal/Source I/O, or fold preflight failure preserves the current text, history, and composition.
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
        // Only an actually empty stack is Unchanged; Core errors above retain
        // their cause instead of being disguised as missing history.
        let Some((edit, prepared)) = restored else {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        };
        let commit = self.publish(permit, &document, prepared, None, Some(edit), &mut folds);
        drop(folds);
        drop(document);
        drop(original);
        Ok(NativeMarkdownPagedInputOutcome::Committed(commit))
    }

    /// Mouse, navigation, and Select All publish selection under this permit without manufacturing text history or a generation.
    ///
    /// # Errors
    /// Active composition, revocation, a split scalar, or Core preparation preserves the complete original state.
    pub fn set_selection(
        self: &Rc<Self>,
        permit: &NativeMarkdownPagedInputPermit,
        selection: MarkdownTextSelection,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedInputOutcome> {
        self.publish_selection(permit, selection, None, current)
    }

    /// Publish navigation direction, wrap affinity, and goal column with one selection serial; identical numeric ranges can still carry new intent.
    /// Update the sole selection slot without changing text generation, adding Undo history, or creating a navigation cache.
    ///
    /// # Errors
    /// As with set_selection, reject an invalid goal before changing Core selection and keep original facts intact.
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

    /// Visual navigation publishes the actual Bidi side with selection serial; changing sides at the same offset revokes old permits.
    ///
    /// # Errors
    /// Use ordinary selection intent semantics without changing text generation/Undo or creating another selection cache.
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

    /// Return the original selection slot's visual side; current reentry must not resurrect geometry it revoked.
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

    /// Read this permit's selection slot; a canonical history memento cannot supply an unknown primary head for multiple ranges.
    ///
    /// # Errors
    /// Reject lost original input/state/foreground authority; these facts do not retain a document, Native object, or Source.
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
        } // current may have cleared geometry without changing canonical offsets; the returned facts must not resurrect it.
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
            (None, None) // An ordered primary range has no known direction, so do not guess upstream/downstream affinity.
        }
    }

    /// Revoke geometry when Source/style/layout coordinates change while retaining logical anchor/head and history.
    pub fn clear_navigation_geometry(&self) {
        self.selection.clear_vertical_goal();
    }

    /// For the same Source, revoke geometry invalidated by selection-style layout while retaining explicit head affinity without recapture.
    pub(in crate::native_surface) fn clear_style_navigation_geometry(&self) {
        self.selection.clear_style_geometry();
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
            // Check original Source first: an invalid goal cannot hide an existing backing failure or change selection.
            document.byte_offset_for_utf16(permit.before.selection.anchor_utf16)?;
            return Err(NativeMarkdownPagedInputError::InvalidRange);
        }
        let mut folds = self
            .folds
            .try_borrow_mut()
            .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
        let mut retained = Vec::with_capacity(folds.len());
        for fold in folds.iter() {
            let hidden = folding::folded_layout_range(&document, fold)?;
            let inside = |position| hidden.start < position && position < hidden.end;
            if !inside(selection.anchor_utf16) && !inside(selection.head_utf16) {
                retained.push(fold.clone());
            }
        } // Reveal folds entered by actual selection endpoints in the same receipt; endpoints outside a spanned fold do not imply reveal.
        let folds_changed = retained.len() != folds.len();
        let fold_revision = if folds_changed {
            permit
                .before
                .fold_revision
                .checked_add(1)
                .ok_or(NativeMarkdownPagedInputError::CounterExhausted)?
        } else {
            permit.before.fold_revision
        };
        document.set_selection(selection)?;
        if intent.is_none()
            && selection == permit.before.selection
            && permit.before.selection_is_directed
            && !folds_changed
        {
            return Ok(NativeMarkdownPagedInputOutcome::Unchanged(permit.before));
        }
        // set_selection checked Source health, idle state, and scalar endpoints; only bounded metadata follows, without Native/current/I/O.
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
        if folds_changed {
            *folds = retained;
            self.fold_revision.set(fold_revision);
        }
        let state = NativeMarkdownPagedInputState {
            selection,
            selection_is_directed: true,
            selection_range_count: 1,
            primary_range: ordered_selection_range(selection),
            selection_serial: serial,
            fold_revision,
            ..permit.before
        };
        drop(folds);
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
                    folds_changed,
                },
            },
        ))
    }

    /// Close preflight reads this admission: an active transaction cannot accept closure that releases its document.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy.get()
    }

    /// Closing releases bounded metadata only; text/mappings stay with the composition root, and old permits/commits lose authority immediately.
    ///
    /// # Errors
    /// Reject while synchronous input is active; do not clear its candidate or report that live input as closed.
    pub fn close(&self) -> Result<()> {
        if self.closed.get() {
            return Ok(());
        } // Repeated closure advances no serial, rebuilds no capacity, and affects no successor owner.
        if self.busy.get() {
            return Err(NativeMarkdownPagedInputError::Busy);
        }
        self.closed.set(true);
        #[cfg(feature = "native-input-trace")]
        drop(self.input_trace.borrow_mut().take()); // Accepted closure removes the payload; a late scope retains only fixed scalar records.
        self.marked.set(None);
        if self.selection.serial() != u64::MAX {
            self.selection.seal();
        }
        // Even an exhausted serial remains permanently closed; neither wrap it nor panic through an internal expect during teardown.
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
    // Explicit AppKit replacement wins. Only NSNotFound can fall back to original
    // marked text and then the unambiguous current primary selection.
    let range = if requested.location != NSNotFound as usize {
        requested
    } else if let Some(marked) = state.marked {
        marked
    } else {
        if !state.selection_is_directed {
            // Unspecified input cannot guess a replacement from multiple ranges; preserve all ranges/text until an explicit action.
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
    // Native asks for an ordered NSRange. Keep original anchor/head separately
    // rather than infer their direction from this projection later.
    NSRange::new(
        selection.anchor_utf16.min(selection.head_utf16),
        selection.anchor_utf16.abs_diff(selection.head_utf16),
    )
}

fn text_boundary(text: &str, at: usize) -> bool {
    // Walk only new input: UTF-8 scalars contribute one/two UTF-16 units, so a
    // low-surrogate position cannot become a valid relative candidate endpoint.
    let mut units = 0;
    for scalar in text.chars() {
        if units == at {
            return true;
        }
        units += scalar.len_utf16();
        if units > at {
            return false;
        } // A position inside a surrogate pair must not become a seemingly valid candidate selection.
    }
    units == at
}

#[cfg(test)]
mod tests;

mod replacements;
mod selection_ranges;
pub use selection_ranges::NATIVE_MARKDOWN_PAGED_SELECTION_RANGE_LIMIT;
