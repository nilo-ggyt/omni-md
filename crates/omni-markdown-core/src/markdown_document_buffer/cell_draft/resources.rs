//! A fixed draft scope observes actual branch/reader-payload release without
//! keeping the document, text tree, segments, or mappings alive.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use super::{MarkdownCellDraftError, MarkdownDocumentBufferError};
use crate::MarkdownOriginalBackingObserver;

pub(super) const MAX_READ_ROOTS: usize = 2;

pub(in crate::markdown_document_buffer) struct Scope {
    pub(super) cancelled: AtomicBool,
    pub(super) serial: AtomicU64,
    pub(super) content_serial: AtomicU64,
    branch_live: AtomicBool,
    read_roots: AtomicUsize,
    peak_read_roots: AtomicUsize,
    delta_bytes: AtomicUsize,
    peak_delta_bytes: AtomicUsize,
    first_source_errno: AtomicI32,
    // Observe only actual mappings created by this raw branch. Empty heap edits
    // allocate no Vec inventory, and inherited canonical mappings are not registered twice.
    raw_backings: Mutex<Vec<MarkdownOriginalBackingObserver>>,
    first_raw_cleanup_errno: AtomicI32,
    first_candidate_cleanup_errno: AtomicI32,
    candidate_cleanup_failed: AtomicBool,
    prepared_commit_live: AtomicBool,
    prepared_commit_bytes: AtomicUsize,
    peak_prepared_commit_bytes: AtomicUsize,
}

impl Scope {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            serial: AtomicU64::new(0),
            content_serial: AtomicU64::new(0),
            branch_live: AtomicBool::new(true),
            read_roots: AtomicUsize::new(0),
            peak_read_roots: AtomicUsize::new(0),
            delta_bytes: AtomicUsize::new(0),
            peak_delta_bytes: AtomicUsize::new(0),
            first_source_errno: AtomicI32::new(0),
            raw_backings: Mutex::new(Vec::new()),
            first_raw_cleanup_errno: AtomicI32::new(0),
            first_candidate_cleanup_errno: AtomicI32::new(0),
            candidate_cleanup_failed: AtomicBool::new(false),
            prepared_commit_live: AtomicBool::new(false),
            prepared_commit_bytes: AtomicUsize::new(0),
            peak_prepared_commit_bytes: AtomicUsize::new(0),
        })
    }

    pub(super) fn record_source(&self, error: MarkdownDocumentBufferError) {
        if let MarkdownDocumentBufferError::OriginalBackingUnavailable(errno) = error {
            // Retain the actual first errno. Cancellation, staleness, or a later
            // successful operation cannot renew a Source that already failed I/O.
            let _ = self.first_source_errno.compare_exchange(
                0,
                errno,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    fn require_live(&self) -> Result<(), MarkdownCellDraftError> {
        let errno = self.first_source_errno.load(Ordering::Acquire);
        if errno != 0 {
            return Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into());
        }
        if self.cancelled.load(Ordering::Acquire) {
            // An old owner may still hold immutable bytes after cancellation,
            // but those bytes grant no continued input or layout authority.
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    pub(super) fn require(&self, serial: u64) -> Result<(), MarkdownCellDraftError> {
        self.require_live()?;
        if self.serial.load(Ordering::Acquire) != serial {
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    pub(super) fn require_content(&self, serial: u64) -> Result<(), MarkdownCellDraftError> {
        self.require_live()?;
        if self.content_serial.load(Ordering::Acquire) != serial {
            // Revoke only old text. Selection or unmark does not create another
            // layout domain for the same unchanged root.
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    pub(super) fn reserve_reader(self: &Arc<Self>) -> Result<ReadClaim, MarkdownCellDraftError> {
        let mut count = self.read_roots.load(Ordering::Acquire);
        loop {
            if count >= MAX_READ_ROOTS {
                // Two actual payloads are the hard limit. Do not grow a Vec, drop
                // another owner, or clear its count early to manufacture a free slot.
                return Err(MarkdownCellDraftError::ReadersBusy);
            }
            match self.read_roots.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // The claim, not an external completion flag, returns this
                    // reserved slot when the final frozen payload actually drops.
                    self.peak_read_roots.fetch_max(count + 1, Ordering::AcqRel);
                    return Ok(ReadClaim(Arc::clone(self)));
                }
                Err(actual) => count = actual,
            }
        }
    }

    pub(super) fn has_readers(&self) -> bool {
        self.read_roots.load(Ordering::Acquire) != 0
    }

    pub(super) fn has_retired_owners(&self) -> bool {
        // A cancelled branch cannot admit canonical writes while any actual
        // reader, prepared payload, or unconfirmed mapping cleanup remains.
        self.has_readers()
            || self.prepared_commit_live.load(Ordering::Acquire)
            || self.candidate_cleanup_failed.load(Ordering::Acquire)
            || !self.raw_backings_released()
    }

    pub(super) fn reserve_commit(self: &Arc<Self>) -> Result<CommitClaim, MarkdownCellDraftError> {
        self.require_live()?;
        if self
            .prepared_commit_live
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Reserve the unique slot before a bounded String or anonymous output
            // is created. Repeated capture of one serial cannot create N commit copies.
            return Err(MarkdownCellDraftError::CommitBusy);
        }
        Ok(CommitClaim {
            scope: Arc::clone(self),
            backing: None,
        })
    }

    /// Register the actual observer before a file first enters raw segments. Keep
    /// no strong mapping edge and do not count inherited canonical segments again.
    pub(super) fn observe_raw_backing(&self, backing: MarkdownOriginalBackingObserver) {
        let mut backings = self.raw_backings.lock().unwrap();
        backings.retain(|previous| {
            // A rolled-back candidate can leave the list only after actual unmap.
            // Its real failure, if any, remains sticky in the original scope.
            self.record_raw_cleanup(previous);
            !previous.unmap_succeeded() || previous.mapped_bytes() != 0
        });
        backings.push(backing);
    }

    /// Observe actual system-operation facts. Raw candidate cleanup failure stays
    /// distinct from the unchanged original Source's text authority.
    fn record_raw_cleanup(&self, backing: &MarkdownOriginalBackingObserver) {
        if let Some(errno) = backing.first_errno() {
            let _ = self.first_raw_cleanup_errno.compare_exchange(
                0,
                errno,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    /// Only release queries traverse these real observers. Text health and layout
    /// reads neither lock this list nor scan mapping state.
    fn raw_backings_released(&self) -> bool {
        let backings = self.raw_backings.lock().unwrap();
        let mut released = true;
        for backing in backings.iter() {
            // Observe every remaining ticket even after one failure, so short
            // circuiting cannot hide another actual file's first errno.
            self.record_raw_cleanup(backing);
            released &=
                backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed();
        }
        released && self.first_raw_cleanup_errno.load(Ordering::Acquire) == 0
    }

    /// Check raw mappings only after branch and final reader owners end. A still
    /// legitimate owner must not be reported as a cleanup failure.
    fn finish_raw_backings(&self) {
        if !self.branch_live.load(Ordering::Acquire) && !self.has_readers() {
            self.raw_backings_released();
        }
    }

    pub(super) fn publish_delta(&self, bytes: usize) {
        // This is actual draft-delta accounting, not RSS or inherited original
        // bytes. Updating its peak retains no text or retired branch payload.
        self.delta_bytes.store(bytes, Ordering::Release);
        self.peak_delta_bytes.fetch_max(bytes, Ordering::AcqRel);
    }
}

pub(super) struct BranchClaim(pub(super) Arc<Scope>);
impl Drop for BranchClaim {
    fn drop(&mut self) {
        // This field must follow the candidate root and independent segment table.
        // Only their actual Drop permits revoking the branch-live fact.
        self.0.delta_bytes.store(0, Ordering::Release);
        self.0.branch_live.store(false, Ordering::Release);
        self.0.cancelled.store(true, Ordering::Release);
        self.0.finish_raw_backings();
    }
}

pub(in crate::markdown_document_buffer) struct ReadClaim(Arc<Scope>);
impl Drop for ReadClaim {
    fn drop(&mut self) {
        // FrozenText places this after its tree, segments, and health backing.
        // Internal snapshot clones must really finish before the slot is returned.
        self.0.read_roots.fetch_sub(1, Ordering::AcqRel);
        self.0.finish_raw_backings();
    }
}

pub(super) struct CommitClaim {
    scope: Arc<Scope>,
    backing: Option<MarkdownOriginalBackingObserver>,
}

impl CommitClaim {
    pub(super) fn allocated(&self, bytes: usize) {
        // Count only heap text capacity actually held by prepared admission.
        // File extent and bytes shared with the original tree are not heap capacity.
        self.scope
            .prepared_commit_bytes
            .store(bytes, Ordering::Release);
        self.scope
            .peak_prepared_commit_bytes
            .fetch_max(bytes, Ordering::AcqRel);
    }

    /// Bind the actual mapped backing immediately after seal. Failed candidate
    /// reads or Inline conversion cannot bypass its final release verification.
    pub(super) fn observe_backing(&mut self, backing: MarkdownOriginalBackingObserver) {
        self.backing = Some(backing);
    }

    /// Remove the observer only after canonical publication takes over this same
    /// mapping or actual release of the temporary Inline mapping has been confirmed.
    pub(super) fn release_backing_ticket(&mut self) {
        self.backing = None;
    }
}

impl Drop for CommitClaim {
    fn drop(&mut self) {
        // PreparedCommit places this field after actual candidate storage. Return
        // the unique slot only after its String or mapping owner has dropped.
        if let Some(backing) = &self.backing {
            if let Some(errno) = backing.first_errno() {
                let _ = self.scope.first_candidate_cleanup_errno.compare_exchange(
                    0,
                    errno,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            if !backing.unmap_succeeded() || backing.mapped_bytes() != 0 || backing.failed() {
                // Replacement already dropped. Preserve the actual failed-release
                // fact; returning an admission slot does not prove munmap succeeded.
                self.scope
                    .candidate_cleanup_failed
                    .store(true, Ordering::Release);
            }
        }
        self.scope.prepared_commit_bytes.store(0, Ordering::Release);
        self.scope
            .prepared_commit_live
            .store(false, Ordering::Release);
    }
}

#[derive(Clone)]
/// Observe actual owner/mapping cleanup without retaining draft text, document,
/// or files. These ownership counts are not a system-memory sample.
pub struct MarkdownCellDraftResourceObserver {
    pub(super) scope: Arc<Scope>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Snapshot branch delta, actual reader payloads and unique prepared String
/// capacity. Capsule clones share a payload without increasing read_roots.
pub struct MarkdownCellDraftResourceSnapshot {
    pub branch_live: bool,
    pub read_roots: usize,
    pub peak_read_roots: usize,
    pub delta_bytes: usize,
    pub peak_delta_bytes: usize,
    pub cancelled: bool,
    pub first_source_errno: Option<i32>,
    pub first_raw_cleanup_errno: Option<i32>,
    pub first_candidate_cleanup_errno: Option<i32>,
    pub prepared_commit_live: bool,
    pub prepared_commit_bytes: usize,
    pub peak_prepared_commit_bytes: usize,
}

impl MarkdownCellDraftResourceObserver {
    #[must_use]
    /// Individual fields are atomic observations. Release requires the actual
    /// branch and all reader payloads to have returned, not merely a cancelled flag.
    pub fn snapshot(&self) -> MarkdownCellDraftResourceSnapshot {
        let errno = self.scope.first_source_errno.load(Ordering::Acquire);
        let raw_errno = self.scope.first_raw_cleanup_errno.load(Ordering::Acquire);
        let candidate_errno = self
            .scope
            .first_candidate_cleanup_errno
            .load(Ordering::Acquire);
        MarkdownCellDraftResourceSnapshot {
            branch_live: self.scope.branch_live.load(Ordering::Acquire),
            read_roots: self.scope.read_roots.load(Ordering::Acquire),
            peak_read_roots: self.scope.peak_read_roots.load(Ordering::Acquire),
            delta_bytes: self.scope.delta_bytes.load(Ordering::Acquire),
            peak_delta_bytes: self.scope.peak_delta_bytes.load(Ordering::Acquire),
            cancelled: self.scope.cancelled.load(Ordering::Acquire),
            first_source_errno: (errno != 0).then_some(errno),
            first_raw_cleanup_errno: (raw_errno != 0).then_some(raw_errno),
            first_candidate_cleanup_errno: (candidate_errno != 0).then_some(candidate_errno),
            prepared_commit_live: self.scope.prepared_commit_live.load(Ordering::Acquire),
            prepared_commit_bytes: self.scope.prepared_commit_bytes.load(Ordering::Acquire),
            peak_prepared_commit_bytes: self
                .scope
                .peak_prepared_commit_bytes
                .load(Ordering::Acquire),
        }
    }

    #[must_use]
    /// After tree, reader and prepared owners end, actual raw-file and untransferred
    /// candidate cleanup must also succeed. The document's real backing observers
    /// retain final munmap responsibility for inherited canonical Source and
    /// successfully transferred Frozen storage.
    pub fn is_released(&self) -> bool {
        let state = self.snapshot();
        !state.branch_live
            && state.read_roots == 0
            && !state.prepared_commit_live
            && state.prepared_commit_bytes == 0
            && !self.scope.candidate_cleanup_failed.load(Ordering::Acquire)
            && self.scope.raw_backings_released()
    }
}

/// A weak nonce retains no candidate branch. Dropping it marks cancellation only;
/// the document must still explicitly drain its unique slot.
pub struct MarkdownCellDraft {
    pub(super) scope: Weak<Scope>,
}
impl Drop for MarkdownCellDraft {
    fn drop(&mut self) {
        if let Some(scope) = self.scope.upgrade() {
            scope.cancelled.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests;
