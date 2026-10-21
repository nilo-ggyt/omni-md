//! A pure Core budget gate: only the caller's next step replenishes the original future, with no external wake registration.
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

/// Retain fixed counters, original cancellation, and backing observations without owning text or the Store.
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
        // This immutable snapshot's original backing supplies its actual I/O poison; purely edited pieces do not gain a fictional FD.
        // Every await checks the original errno before admitting work, so cross-thread cancellation cannot hide known source damage.
        if let Some(errno) = self.backing.as_ref().and_then(|value| value.first_errno()) {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        #[cfg(feature = "s2-table-window-prototype")]
        if let Some(draft) = &self.draft {
            // Retain only weak scope/original contentSerial, without Source or the active draft. Actual source I/O failure stays first.
            // Metadata-only awaits must also observe revocation rather than waiting for another text borrow to stop work.
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
        // Replace this explicit quantum's remaining allowance and progress; cumulative maximum source borrowing remains independently observable.
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
        // Record after actual copy but before lease return; a subsequent return failure cannot erase work already performed.
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

/// Ready admits one record/resource operation attempt; Pending performs no Store call and does not unwind a future with modified rule state.
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
            return Poll::Pending; // The original worker explicitly replenishes the next quantum; no timer, registered waker, or busy wait is created here.
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

/// Admit the actual byte bound before source access, without reading 4 KiB and charging only one byte afterward.
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
        // One byte completes at most one scalar, so use the smaller remaining allowance; incomplete UTF8 still advances under a one-byte quota.
        let count = self.maximum.min(budget.bytes).min(budget.scalars).min(4096);
        budget.bytes -= count;
        self.gate.remaining.set(budget);
        Poll::Ready(Ok(count))
    }
}

/// A scalar may span several one-byte quanta; its Unicode rules run only after complete decoding and scalar admission.
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
