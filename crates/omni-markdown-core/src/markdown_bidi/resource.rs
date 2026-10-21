//! Actual return facts for UBA anonymous files and fixed pages; observers retain atomic facts, without owning text, files, or caches.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering},
};

pub(super) struct ResourceState {
    file_created: AtomicBool,
    // Visible analysis retains at most the previous/current part; every actual File acquisition/close belongs to this same scope.
    open_files: AtomicUsize,
    // Zero means no reported close, one comes only from actual success, and a negative value preserves the actual failed-close errno.
    close_status: AtomicI32,
    cache_bytes: AtomicUsize,
    peak_cache_bytes: AtomicUsize,
    page_loads: AtomicU64,
}

impl ResourceState {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            file_created: AtomicBool::new(false),
            open_files: AtomicUsize::new(0),
            close_status: AtomicI32::new(0),
            cache_bytes: AtomicUsize::new(0),
            peak_cache_bytes: AtomicUsize::new(0),
            page_loads: AtomicU64::new(0),
        })
    }

    pub(super) fn file_acquired(&self) {
        self.open_files.fetch_add(1, Ordering::AcqRel);
        self.file_created.store(true, Ordering::Release);
    }

    pub(super) fn observe(self: &Arc<Self>) -> MarkdownBidiResourceObserver {
        MarkdownBidiResourceObserver(Some(Arc::clone(self)))
    }

    pub(super) fn close_returned(&self, status: i32) {
        if status == 1 {
            self.open_files.fetch_sub(1, Ordering::AcqRel);
            // A later file's successful close cannot overwrite a previous file's actual failure errno.
            let _ = self
                .close_status
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        } else {
            // Failed close is not evidence that an open-file claim was returned; preserve the first negative fact across the shared scope.
            let _ = self
                .close_status
                .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                    (old >= 0).then_some(status)
                });
        }
    }

    pub(super) fn page_allocated(&self, bytes: usize) {
        let live = self.cache_bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.peak_cache_bytes.fetch_max(live, Ordering::AcqRel);
    }

    pub(super) fn page_deallocated(&self, bytes: usize) {
        self.cache_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }

    /// Charge every successful actual page read immediately; a pending part or later cancellation cannot erase this I/O.
    pub(super) fn page_loaded(&self) {
        self.page_loads.fetch_add(1, Ordering::AcqRel);
    }

    /// Both live parts and retired parts in this scope share accumulated actual page reads.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn page_loads(&self) -> u64 {
        self.page_loads.load(Ordering::Acquire)
    }
}

/// Uniform certificates create no file/page; Paged observations share fixed atomic state with actual original File and Box return events.
#[derive(Clone)]
pub struct MarkdownBidiResourceObserver(Option<Arc<ResourceState>>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownBidiResourceSnapshot {
    pub file_created: bool,
    pub file_open: bool,
    pub close_succeeded: bool,
    pub close_errno: Option<i32>,
    pub cache_bytes: usize,
    pub peak_cache_bytes: usize,
}

impl MarkdownBidiResourceObserver {
    pub(super) fn unallocated() -> Self {
        Self(None)
    }

    #[must_use]
    pub fn snapshot(&self) -> MarkdownBidiResourceSnapshot {
        let Some(state) = &self.0 else {
            return MarkdownBidiResourceSnapshot {
                file_created: false,
                file_open: false,
                close_succeeded: false,
                close_errno: None,
                cache_bytes: 0,
                peak_cache_bytes: 0,
            };
        }; // Never allocated is distinct from an actual successful close, so callers can distinguish these resource paths.
        let created = state.file_created.load(Ordering::Acquire);
        let status = state.close_status.load(Ordering::Acquire);
        let open_files = state.open_files.load(Ordering::Acquire);
        MarkdownBidiResourceSnapshot {
            file_created: created,
            file_open: open_files != 0,
            close_succeeded: status == 1 && open_files == 0,
            close_errno: (status < 0).then(|| -status),
            cache_bytes: state.cache_bytes.load(Ordering::Acquire),
            peak_cache_bytes: state.peak_cache_bytes.load(Ordering::Acquire),
        }
    }

    #[must_use]
    pub fn is_released(&self) -> bool {
        // Release requires no remaining owned file/cache and no failed-close fact; it is not a claim about OS RSS or Source backing lifetime.
        let actual = self.snapshot();
        !actual.file_open && actual.cache_bytes == 0 && actual.close_errno.is_none()
    }

    #[cfg(test)]
    pub(super) fn closed(&self) -> bool {
        self.snapshot().close_succeeded
    }
}
