//! 纯Core预算闸门；只有调用者再次step才会给原Future补充配额，不注册外部唤醒者。
use super::*;
use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

#[derive(Clone, Copy)]
pub(in crate::markdown_bidi) enum Operation {
    Read,
    Write,
    Resource,
}

/// 只含固定计数、原取消位和原backing的原子观察者，不持正文或Store。
pub(in crate::markdown_bidi) struct Gate {
    remaining: Cell<MarkdownBidiPrepareBudget>,
    progress: Cell<MarkdownBidiPrepareStep>,
    maximum_borrow: Cell<usize>,
    backing: Option<crate::MarkdownOriginalBackingObserver>,
    #[cfg(feature = "s2-table-window-prototype")]
    draft: Option<crate::markdown_document_buffer::MarkdownCellDraftSourceAuthority>,
    cancellation: MarkdownBidiCancellation,
    #[cfg(all(test, feature = "paged-layout-breaks"))]
    fail_flush: Cell<bool>,
}
impl Gate {
    pub(in crate::markdown_bidi) fn new(
        snapshot: &MarkdownTextSnapshot,
        cancellation: MarkdownBidiCancellation,
    ) -> Rc<Self> {
        Rc::new(Self {
            remaining: Cell::new(MarkdownBidiPrepareBudget::empty()),
            progress: Cell::new(MarkdownBidiPrepareStep::pending()),
            maximum_borrow: Cell::new(0),
            backing: snapshot.original_backing_observer(),
            #[cfg(feature = "s2-table-window-prototype")]
            draft: snapshot.cell_draft_source_authority(),
            cancellation,
            #[cfg(all(test, feature = "paged-layout-breaks"))]
            fail_flush: Cell::new(false),
        })
    }

    pub(in crate::markdown_bidi) fn source_health(&self) -> Result<(), BidiError> {
        // 同一个immutable snapshot只有这个原backing可产生读取I/O毒化；纯编辑段没有假FD。
        // 每个await在消费预算前也查原errno，不能让跨线程取消覆盖已知损坏。
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(draft) = &self.draft {
            // 只Weak scope与原contentSerial，不保Source或活动草稿。源I/O首错仍先检查；
            // 元数据only阶段每个await也须看到真正撤权，不能等下一次正文借用才停机。
            draft.health()?;
        }
        Ok(())
    }
    pub(in crate::markdown_bidi) fn health(&self) -> Result<(), BidiError> {
        self.source_health()?;
        self.cancellation.check()
    }

    #[cfg(all(test, feature = "paged-layout-breaks"))]
    pub(super) fn fail_next_flush(&self) {
        self.fail_flush.set(true);
    }

    #[cfg(all(test, feature = "paged-layout-breaks"))]
    pub(super) fn take_flush_failure(&self) -> bool {
        self.fail_flush.replace(false)
    }

    pub(in crate::markdown_bidi) fn replenish(&self, budget: MarkdownBidiPrepareBudget) {
        self.remaining.set(budget);
        self.progress.set(MarkdownBidiPrepareStep::pending());
    }
    pub(in crate::markdown_bidi) fn progress(&self) -> MarkdownBidiPrepareStep {
        self.progress.get()
    }
    pub(in crate::markdown_bidi) fn maximum_borrow(&self, count: usize) {
        self.maximum_borrow
            .set(self.maximum_borrow.get().max(count));
        let mut progress = self.progress.get();
        progress.source_borrow_max_bytes = progress.source_borrow_max_bytes.max(count);
        self.progress.set(progress);
    }
    pub(in crate::markdown_bidi) fn source_borrow_max(&self) -> usize {
        self.maximum_borrow.get()
    }
    pub(in crate::markdown_bidi) fn bytes_read(&self, count: usize) {
        // 这在真实copy发生后、lease归还前记录，即使随后归还失败也不把工作洗成0。
        let mut progress = self.progress.get();
        progress.bytes += count;
        self.progress.set(progress);
    }
    pub(in crate::markdown_bidi) fn decoded_scalar(&self) {
        let mut progress = self.progress.get();
        progress.scalars += 1;
        self.progress.set(progress);
    }

    pub(in crate::markdown_bidi) fn operation(self: &Rc<Self>, kind: Operation) -> RecordPermit {
        RecordPermit {
            gate: Rc::clone(self),
            kind,
        }
    }
    pub(in crate::markdown_bidi) fn source(self: &Rc<Self>, maximum: usize) -> SourcePermit {
        SourcePermit {
            gate: Rc::clone(self),
            maximum,
        }
    }
    pub(in crate::markdown_bidi) fn scalar(self: &Rc<Self>) -> ScalarPermit {
        ScalarPermit {
            gate: Rc::clone(self),
        }
    }
}

/// Ready仅允许一次真实32B操作；Pending不执行Store调用，也不展开已修改规则的Future。
pub(in crate::markdown_bidi) struct RecordPermit {
    gate: Rc<Gate>,
    kind: Operation,
}
impl Future for RecordPermit {
    type Output = Result<(), BidiError>;
    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        if let Err(error) = self.gate.health() {
            return Poll::Ready(Err(error));
        }
        let mut budget = self.gate.remaining.get();
        if budget.metadata_ops == 0 {
            return Poll::Pending; // 原worker下一量子显式补票；没有timer/waker或忙等。
        }
        budget.metadata_ops -= 1;
        self.gate.remaining.set(budget);
        let mut progress = self.gate.progress.get();
        progress.metadata_ops += 1;
        match self.kind {
            Operation::Read => progress.record_reads += 1,
            Operation::Write => progress.record_writes += 1,
            Operation::Resource => progress.resource_ops += 1,
        }
        self.gate.progress.set(progress);
        Poll::Ready(Ok(()))
    }
}

/// 源批量在访问之前取得真实byte上限；不会先读4KiB后只给调用方记1byte。
pub(in crate::markdown_bidi) struct SourcePermit {
    gate: Rc<Gate>,
    maximum: usize,
}
impl Future for SourcePermit {
    type Output = Result<usize, BidiError>;
    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        if let Err(error) = self.gate.health() {
            return Poll::Ready(Err(error));
        }
        let mut budget = self.gate.remaining.get();
        if budget.bytes == 0 || budget.scalars == 0 {
            return Poll::Pending;
        }
        // 每byte至多完成一个scalar，保守取两者余额；跨UTF8残余仍可在1byte预算下前进。
        let count = self.maximum.min(budget.bytes).min(budget.scalars).min(4096);
        budget.bytes -= count;
        self.gate.remaining.set(budget);
        Poll::Ready(Ok(count))
    }
}

/// 一个scalar可跨多个1byte量子；只有解码完成且取得scalar配额才执行其Unicode规则。
pub(in crate::markdown_bidi) struct ScalarPermit {
    gate: Rc<Gate>,
}
impl Future for ScalarPermit {
    type Output = Result<(), BidiError>;
    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        if let Err(error) = self.gate.health() {
            return Poll::Ready(Err(error));
        }
        let mut budget = self.gate.remaining.get();
        if budget.scalars == 0 {
            return Poll::Pending;
        }
        budget.scalars -= 1;
        self.gate.remaining.set(budget);
        Poll::Ready(Ok(()))
    }
}
