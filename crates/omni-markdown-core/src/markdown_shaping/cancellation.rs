//! Scalar-only cross-thread cancellation requests; the original worker retains exclusive job and paged-array ownership.

use super::{
    MarkdownShapingError as Error, MarkdownShapingJob, MarkdownShapingStatus as Status, Result,
};

/// Requesting stop does not prove completion; callers join the original worker and check actual resource receipts.
#[derive(Clone, Default)]
pub struct MarkdownShapingCancellation(harfrust::CancellationToken);

impl MarkdownShapingCancellation {
    /// Permanently request this original job to stop, without resetting, recapturing Source, or granting readiness.
    pub fn request(&self) {
        self.0.request();
    }
    /// Observe the request bit only, not whether the shaping algorithm has already seen it.
    #[must_use]
    pub fn requested(&self) -> bool {
        self.0.requested()
    }
}

impl MarkdownShapingJob {
    /// Share the original scalar request bit without letting Native retain the worker's document, Font, or glyph arrays.
    #[must_use]
    pub fn cancellation(&self) -> MarkdownShapingCancellation {
        self.cancellation.clone()
    }

    pub(super) fn cancelled<T>(&mut self) -> Result<T> {
        let backing = self
            .payload
            .as_ref()
            .and_then(|payload| payload.source.original_backing_observer());
        self.status = Status::Cancelled;
        drop(self.payload.take()); // End owner borrows before actual mapping return, without publishing partial glyph output.
        if let Some(errno) = backing.as_ref().and_then(|observer| observer.first_errno()) {
            self.status = Status::Poisoned;
            return Err(
                super::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        } // Actual failure when the final Source owner unmaps cannot masquerade as clean cancellation.
        if let Some(error) = self.resources.paging.first_error() {
            self.status = Status::Poisoned;
            return Err(harfrust::ShapeError::Paging {
                operation: error.operation as u32,
                errno: error.errno,
            }
            .into());
        } // Actual Buffer Drop close/unmap errors retain the original paging scope's first cause rather than being hidden by Cancelled.
        Err(Error::Cancelled)
    }

    pub(super) fn cooperative_control(&self) -> Result<harfrust::CooperativeControl> {
        let payload = self.payload.as_ref().ok_or(Error::Poisoned)?;
        let observer = payload.source.original_backing_observer();
        #[cfg(test)]
        let hook = self.cancellation_hook.clone();
        // Capture only original backing-facts receipts, without retaining Source, the inode, mmap, or Native objects.
        let health: harfrust::CooperativeSourceHealth = std::sync::Arc::new(move |point| {
            #[cfg(test)]
            if let Some(hook) = &hook {
                hook(point);
            }
            #[cfg(not(test))]
            let _ = point;
            observer
                .as_ref()
                .and_then(|observer| observer.first_errno())
        });
        Ok(harfrust::CooperativeControl::new(
            self.cancellation.0.clone(),
            payload.buffer.paging_resource_observer(),
            Some(health),
        ))
    }
}
