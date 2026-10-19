//! 文本提交前的纯变化观察合同；原生通知仍使用独立TextKit包络，不能反过来代替真实变化。

use std::convert::Infallible;
use std::ops::Range;

use super::change_journal::{self, Record};
use super::*;

pub use change_journal::{MarkdownChangeJournalError, MarkdownChangeJournalIoFailure};

#[derive(Clone, Debug, Eq, PartialEq)]
/// 真实旧坐标的一项变化；调用顺序保留相邻replacement及同位置插入的独立边界。
pub struct MarkdownTextChange {
    pub old_utf16_range: Range<usize>,
    pub inserted_utf16_len: usize,
}

#[derive(Debug)]
/// 正文、日志和观察者错误保留独立typed原因；双失败保存首因而不产生递归derive约束。
pub enum MarkdownObservedEditError<E> {
    Document(MarkdownDocumentBufferError),
    Journal(MarkdownChangeJournalError),
    Observer(E),
    RollbackUnconfirmed {
        cause: Box<MarkdownObservedEditError<E>>,
        cleanup: MarkdownChangeJournalError,
    },
}
impl<E> From<MarkdownDocumentBufferError> for MarkdownObservedEditError<E> {
    fn from(error: MarkdownDocumentBufferError) -> Self {
        Self::Document(error)
    }
}
impl<E> From<MarkdownChangeJournalError> for MarkdownObservedEditError<E> {
    fn from(error: MarkdownChangeJournalError) -> Self {
        Self::Journal(error)
    }
}
impl<E: std::fmt::Display> std::fmt::Display for MarkdownObservedEditError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(error) => error.fmt(formatter),
            Self::Journal(error) => error.fmt(formatter),
            Self::Observer(error) => error.fmt(formatter),
            // 同一已知impl递归展示首因，不要求Box<Self>:Display作为自身的额外where约束。
            Self::RollbackUnconfirmed { cause, cleanup } => write!(formatter, "{cause}；{cleanup}"),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for MarkdownObservedEditError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Document(error) => Some(error),
            Self::Journal(error) => Some(error),
            Self::Observer(error) => Some(error),
            Self::RollbackUnconfirmed { cause, .. } => Some(cause.as_ref()),
        }
    }
}

/// Undo/Redo在无历史时返回None；有历史则同时返回通知包络与观察结果，错误不移动任何票据。
pub type MarkdownObservedHistoryResult<O> = Result<
    Option<(
        MarkdownHistoryTextEdit,
        <O as MarkdownTextChangeObserver>::Output,
    )>,
    MarkdownObservedEditError<<O as MarkdownTextChangeObserver>::Error>,
>;

/// 空替换流没有通知包络，但观察者仍可完成纯候选校验；两者的缺省语义不能混淆。
pub type MarkdownObservedReplacementsResult<O> = Result<
    (
        Option<MarkdownHistoryTextEdit>,
        <O as MarkdownTextChangeObserver>::Output,
    ),
    MarkdownObservedEditError<<O as MarkdownTextChangeObserver>::Error>,
>;

/// 仅借候选root和已有canonical source，不是第二份文档、快照正文或可逃逸的原生对象。
pub struct MarkdownPreparedText<'a> {
    pub(super) document: &'a MarkdownDocumentBuffer,
    pub(super) root: &'a PieceTreeLink,
}

impl MarkdownPreparedText<'_> {
    /// 只读候选聚合长度，不访问映射正文或借用可变文档状态。
    #[must_use]
    pub fn utf16_len(&self) -> usize {
        piece_tree_metrics(self.root).utf16_len
    }

    /// 在实际候选树上校验scalar边界并定位行；fold header_line可在提交前一并准备。
    ///
    /// # Errors
    /// 越界或surrogate内部位置拒绝；不临时交换document.root，也不复制正文。
    pub fn line_for_utf16(&self, position: usize) -> Result<usize, MarkdownDocumentBufferError> {
        let byte = byte_offset_for_utf16_in_tree(self.document, self.root.as_deref(), position, 0)?;
        Ok(metrics_for_tree_prefix(self.document, self.root.as_deref(), byte)?.line_breaks)
    }
}

/// 回调只可执行纯Rust，不得调用Native、重入document或提前发布前台fold。
///
/// changed可流式准备至多256个fold；finish在完整输入/候选选区验证后执行并可以失败。
/// Output须是拥有的结果，不能持有PreparedText借用。原owner/root/代次权限仍由Native调用方确认。
pub trait MarkdownTextChangeObserver {
    type Output;
    type Error;

    fn changed(&mut self, change: MarkdownTextChange) -> Result<(), Self::Error>;
    fn finish(
        self,
        candidate: MarkdownPreparedText<'_>,
        selection_after: MarkdownTextSelection,
    ) -> Result<Self::Output, Self::Error>;
}

pub(super) struct NoopObserver;
impl MarkdownTextChangeObserver for NoopObserver {
    type Output = ();
    type Error = Infallible;

    fn changed(&mut self, _: MarkdownTextChange) -> Result<(), Infallible> {
        Ok(()) // 旧API也必须写真实journal；这里只省略外部派生结果。
    }
    fn finish(
        self,
        _: MarkdownPreparedText<'_>,
        _: MarkdownTextSelection,
    ) -> Result<(), Infallible> {
        Ok(())
    }
}

pub(super) fn emit<O: MarkdownTextChangeObserver>(
    document: &mut MarkdownDocumentBuffer,
    observer: &mut O,
    range: Range<usize>,
    inserted: usize,
) -> Result<(), MarkdownObservedEditError<O::Error>> {
    let removed = range
        .end
        .checked_sub(range.start)
        .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
    if removed == 0 && inserted == 0 {
        // CM标准ChangeSet过滤真正空段；不能凭delete user event为内部fold捏造一次删除。
        return Ok(());
    }
    observer
        .changed(MarkdownTextChange {
            old_utf16_range: range.clone(),
            inserted_utf16_len: inserted,
        })
        .map_err(MarkdownObservedEditError::Observer)?;
    document.change_journal.push(Record {
        start: range.start,
        removed,
        inserted,
    })?;
    Ok(())
}

pub(super) fn legacy_error(
    error: MarkdownObservedEditError<Infallible>,
) -> MarkdownDocumentBufferError {
    match error {
        MarkdownObservedEditError::Document(error) => error,
        MarkdownObservedEditError::Observer(never) => match never {},
        MarkdownObservedEditError::Journal(MarkdownChangeJournalError::BudgetExceeded) => {
            MarkdownDocumentBufferError::ChangeJournalLimitExceeded
        }
        MarkdownObservedEditError::Journal(_) => {
            MarkdownDocumentBufferError::ChangeJournalUnavailable
        }
        MarkdownObservedEditError::RollbackUnconfirmed { .. } => {
            MarkdownDocumentBufferError::ChangeJournalCleanupUnconfirmed
        }
    }
}

#[cfg(test)]
mod tests;
