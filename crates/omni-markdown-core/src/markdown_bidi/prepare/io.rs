//! The asynchronous budget bridge for the same Store; await preserves Rust locals without changing actual page or record formats.
use super::*;
use cooperate::{Gate, Operation};
use std::rc::Rc;
use store::DiskStack;

pub(in crate::markdown_bidi) struct CooperativeStore {
    pub(in crate::markdown_bidi) raw: Store,
    pub(in crate::markdown_bidi) gate: Rc<Gate>,
}
impl CooperativeStore {
    pub(in crate::markdown_bidi) async fn new(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: &Arc<resource::ResourceState>,
        gate: Rc<Gate>,
    ) -> Result<Self, BidiError> {
        gate.operation(Operation::Resource).await?;
        let mut raw = Store::new_prepared_empty(capacity, budget, cancellation, resources)?;
        for _ in 0..budget.pages {
            // Allocate actual pages one at a time under the original fixed budget; at suspension the FD already belongs to this Store, not a second I/O owner.
            gate.operation(Operation::Resource).await?;
            raw.allocate_prepared_page()?;
        }
        gate.health()?;
        raw.healthy()?;
        Ok(Self { raw, gate })
    }
    pub(in crate::markdown_bidi) fn healthy(&self) -> Result<(), BidiError> {
        // Original Source failure precedes Store cancellation/storage checks, including operations that only touch metadata.
        self.gate.source_health()?;
        self.raw.healthy()
    }
    pub(in crate::markdown_bidi) async fn flush(&mut self) -> Result<(), BidiError> {
        self.healthy()?;
        for index in 0..self.raw.prepared_page_count() {
            if self.raw.prepared_page_dirty(index) {
                self.gate.operation(Operation::Resource).await?;
                #[cfg(all(test, feature = "paged-layout-breaks"))]
                if self.gate.take_flush_failure() {
                    // Preserve the existing failure case: perform actual prefix pwrite before returning error, rather than only setting poison.
                    self.raw.fail_next_write_after_prefix(7);
                }
                self.raw.flush_prepared_page(index)?;
            }
        }
        self.healthy() // Final preparation may verify Ready only after all original writes actually succeeded.
    }
    pub(in crate::markdown_bidi) fn into_inner(self) -> Store {
        self.raw
    }
    pub(in crate::markdown_bidi) async fn stack_push(
        &mut self,
        stack: &mut DiskStack,
        first: u32,
        last: u32,
    ) -> Result<(), BidiError> {
        self.gate.operation(Operation::Write).await?;
        stack.push(&mut self.raw, first, last)
    }
    pub(in crate::markdown_bidi) async fn stack_top(
        &mut self,
        stack: &DiskStack,
    ) -> Result<Option<(u32, u32)>, BidiError> {
        self.healthy()?;
        if stack.len() == 0 {
            return Ok(None); // An empty stack has no actual record, so do not charge or report an invented read.
        }
        self.gate.operation(Operation::Read).await?;
        stack.top(&mut self.raw)
    }
    pub(in crate::markdown_bidi) async fn stack_pop(
        &mut self,
        stack: &mut DiskStack,
    ) -> Result<Option<(u32, u32)>, BidiError> {
        self.healthy()?;
        if stack.len() == 0 {
            return Ok(None);
        }
        self.gate.operation(Operation::Read).await?;
        stack.pop(&mut self.raw) // The original pop reads its top once, then changes depth only after success.
    }
}

// The original codec remains the sole 32-byte format authority; budget suspension occurs before each actual operation.
// Compiler-owned future locals retain temporary records, so nested lookahead/backfill resumes without unwinding and replaying modified processing classes.
macro_rules! prepared_record {
    ($record:ty) => {
        impl $record {
            pub(in crate::markdown_bidi) async fn read_prepared(
                store: &mut CooperativeStore,
                id: u32,
            ) -> Result<Self, BidiError> {
                store.gate.operation(Operation::Read).await?;
                Self::read(&mut store.raw, id)
            }
            pub(in crate::markdown_bidi) async fn write_prepared(
                self,
                store: &mut CooperativeStore,
                id: u32,
            ) -> Result<(), BidiError> {
                store.gate.operation(Operation::Write).await?;
                self.write(&mut store.raw, id)
            }
        }
    };
}
prepared_record!(Text);
prepared_record!(Paragraph);
prepared_record!(Run);
prepared_record!(Sequence);
