//! Bounded read-only leases over the canonical anonymous snapshot. A complete virtual
//! mapping never grants a whole-document slice or ownership of all resident pages.

use std::fs::File;
use std::ops::{Deref, Range};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Mapping failures remain available without optional document or Unicode features,
/// so the default workspace can reject actual I/O failures at its own boundary.
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum OriginalBackingError {
    #[error("原始映射读取范围无效")]
    RangeOutsideMapping,
    #[error("原始映射不可用（系统错误 {0}）")]
    Unavailable(i32),
    #[error("同时读取的原始窗口过多")]
    LeaseLimitExceeded,
}

pub(crate) const ORIGINAL_READ_BYTES: usize = 4096;
// Concurrent and recursive visitors share one admission limit. The directory lock
// never crosses a visitor, which permits bounded recursion without self-deadlock.
const MAX_ACTIVE_LEASES: usize = 32;

mod window;
use window::{LeasePins, MANAGED_WINDOW_BYTES, WindowState};

#[derive(Default)]
struct BackingFacts {
    mapped_bytes: AtomicUsize,
    active_leases: AtomicUsize,
    poison: AtomicI32,
    managed_bytes: AtomicUsize,
    peak_managed_bytes: AtomicUsize,
    reclaim_attempts: AtomicUsize,
    reclaim_successes: AtomicUsize,
    unmap_attempted: AtomicBool,
    unmap_succeeded: AtomicBool,
    #[cfg(any(test, feature = "original-backing-reclaim-probe"))]
    fail_next_reclaim: AtomicBool,
}

#[derive(Clone)]
/// Observes fixed VM facts without retaining a mapping, file, document, or history.
pub struct MarkdownOriginalBackingObserver {
    facts: Arc<BackingFacts>,
}

impl MarkdownOriginalBackingObserver {
    #[must_use]
    /// True only after the final mapping owner calls `munmap` and the kernel accepts it.
    /// Reaching `Drop` alone is never a successful release receipt.
    pub fn unmap_succeeded(&self) -> bool {
        self.facts.unmap_succeeded.load(Ordering::Acquire)
    }

    #[must_use]
    /// Returns the extent still owned by this mapping; failed cleanup keeps that extent
    /// visible instead of reporting an invented zero.
    pub fn mapped_bytes(&self) -> usize {
        self.facts.mapped_bytes.load(Ordering::Acquire)
    }

    #[must_use]
    /// Required eviction or final unmap failure stays sticky. Returning a lease pin
    /// does not by itself prove physical page reclamation.
    pub fn failed(&self) -> bool {
        self.facts.poison.load(Ordering::Acquire) != 0
    }

    /// Reports this owner's managed page budget, not process RSS or total product memory.
    #[must_use]
    pub fn managed_resident_bytes(&self) -> usize {
        self.facts.managed_bytes.load(Ordering::Acquire)
    }
    /// Reports the peak admitted slots, bounded to 64 KiB. Process RSS requires an
    /// independent kernel measurement.
    #[must_use]
    pub fn peak_managed_resident_bytes(&self) -> usize {
        self.facts.peak_managed_bytes.load(Ordering::Acquire)
    }
    /// Counts actual `msync` calls rather than treating lease completion as reclamation.
    #[must_use]
    pub fn reclaim_attempts(&self) -> usize {
        self.facts.reclaim_attempts.load(Ordering::Acquire)
    }
    /// Counts successful kernel `msync` returns; an injected sticky error remains
    /// independently observable through `first_errno`.
    #[must_use]
    pub fn reclaim_successes(&self) -> usize {
        self.facts.reclaim_successes.load(Ordering::Acquire)
    }
    /// Requests one synthetic `EIO` after the next successful real
    /// `MS_SYNC | MS_INVALIDATE` call.
    ///
    /// Only tests and an explicit diagnostic feature expose this operation. It does
    /// not claim a kernel failure or modify thread-local errno.
    /// Setting the flag does not evict pages, revoke active leases, or close a file.
    /// Cache hits leave the request intact; only the original owner consumes it
    /// after a successful real `msync`, then permanently records failure. A real
    /// kernel error retains precedence as the first cause.
    /// The observer retains only atomic facts. Once the source owner is gone, this
    /// flag cannot retain a source, mapping, or file or schedule further work.
    /// Final unmap success and injected sticky failure are separate facts; neither
    /// substitutes for the other.
    #[cfg(any(test, feature = "original-backing-reclaim-probe"))]
    pub fn fail_next_reclaim_for_test(&self) {
        self.facts.fail_next_reclaim.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn first_errno(&self) -> Option<i32> {
        match self.facts.poison.load(Ordering::Acquire) {
            0 => None,
            error => Some(error),
        }
    }
}

/// Exposes no `Deref` or whole-slice API. An active lease permits at most 4 KiB
/// of original bytes, while this owner alone controls the mapped extent.
pub(crate) struct OriginalBacking {
    address: NonNull<u8>,
    length: usize,
    page_size: usize,
    facts: Arc<BackingFacts>,
    mapped: bool,
    // Every document/snapshot of this frozen inode shares one fixed directory.
    // Its lock is released before invoking any document visitor.
    windows: Mutex<WindowState>,
    window_slots: usize,
}

// SAFETY: The private inode is frozen with no writers; its mapping stays stable
// until the final owner drops, and only bounded immutable borrows are exposed.
// `msync` neither changes bytes nor revokes the address. Active leases borrow the
// owner, so Rust prevents their use concurrently with its final `munmap`.
unsafe impl Send for OriginalBacking {}
// SAFETY: A mutex serializes directory changes and observer facts are atomic.
// Bytes are immutable; this owner never evicts a slot while a live lease pins it.
unsafe impl Sync for OriginalBacking {}

impl OriginalBacking {
    /// Acquires the virtual mapping used by bounded leases, without exposing a full slice.
    ///
    /// # Safety
    /// `file` must be a private anonymous inode whose length the factory has validated.
    /// All writers must have ended, with no remaining write or truncate handles.
    /// The `immutable_snapshot` factory proves this when freezing a private copy or
    /// consuming exclusive anonymous output. A mutable user file is not a valid input.
    pub(crate) unsafe fn map(file: File, length: usize) -> Result<Self, OriginalBackingError> {
        if length == 0 {
            // Empty input owns no mapping; callers represent that state with `None`.
            return Err(OriginalBackingError::RangeOutsideMapping);
        }
        // SAFETY: `sysconf` transfers no ownership; it only queries the page-size fact
        // required for alignment and a finite directory capacity.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_size = usize::try_from(page_size)
            .ok()
            .filter(|size| {
                size.is_power_of_two()
                    && (ORIGINAL_READ_BYTES..=MANAGED_WINDOW_BYTES).contains(size)
            })
            .ok_or(OriginalBackingError::Unavailable(libc::EINVAL))?;
        // SAFETY: The anonymous frozen-file factory has validated the extent and ended
        // all writers. This owner alone releases the acquired read-only mapping.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            // Preserve the kernel errno. No mapping was acquired, so no release receipt
            // or partially initialized owner may be published.
            return Err(OriginalBackingError::Unavailable(last_errno()));
        }
        let Some(address) = NonNull::new(raw.cast::<u8>()) else {
            // Rust slices require a non-null address. Even a kernel-provided zero address
            // must be compensated before rejecting the acquired mapping.
            // SAFETY: This branch owns an unpublished mapping of `length` bytes, with
            // no live leases or external references.
            if unsafe { libc::munmap(raw, length) } != 0 {
                // Report failed VM compensation rather than implying that the address
                // error also released the mapping.
                return Err(OriginalBackingError::Unavailable(last_errno()));
            }
            return Err(OriginalBackingError::Unavailable(libc::EINVAL));
        };
        // The VM mapping keeps the anonymous inode alive. Release the original
        // descriptor now instead of retaining an idle descriptor.
        let fd = file.into_raw_fd();
        // SAFETY: `into_raw_fd` transfers the sole descriptor, closed exactly once;
        // the mapping already holds an independent VM reference.
        if unsafe { libc::close(fd) } != 0 {
            let errno = last_errno();
            // SAFETY: Acquisition failed before publication or leases; release the VM
            // resource already acquired by this local operation.
            if unsafe { libc::munmap(address.as_ptr().cast(), length) } != 0 {
                // Failed VM compensation is a required cleanup failure and must remain
                // visible instead of being hidden by the earlier descriptor error.
                return Err(OriginalBackingError::Unavailable(last_errno()));
            }
            return Err(OriginalBackingError::Unavailable(errno));
        }
        let facts = Arc::new(BackingFacts::default());
        facts.mapped_bytes.store(length, Ordering::Release);
        Ok(Self {
            address,
            length,
            page_size,
            facts,
            mapped: true,
            windows: Mutex::new(WindowState::default()),
            window_slots: MANAGED_WINDOW_BYTES / page_size,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.length
    }

    /// Native documents and release tests observe these same atomic facts without
    /// extending the lifetime of the file or mapping.
    #[cfg(any(feature = "native-markdown-candidate", test))]
    pub(crate) fn observer(&self) -> MarkdownOriginalBackingObserver {
        MarkdownOriginalBackingObserver {
            facts: Arc::clone(&self.facts),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_reclaim_for_test(&self) {
        self.observer().fail_next_reclaim_for_test(); // Reuse the same diagnostic bit; no separate failure algorithm is installed.
    }

    pub(crate) fn health(&self) -> Result<(), OriginalBackingError> {
        match self.facts.poison.load(Ordering::Acquire) {
            0 => Ok(()),
            // The first failure permanently revokes later reads. New metadata or a
            // cache hit cannot restore authority over a failed mapping.
            errno => Err(OriginalBackingError::Unavailable(errno)),
        }
    }

    fn fail(&self, errno: i32) -> OriginalBackingError {
        let errno = if errno == 0 { libc::EIO } else { errno };
        let _ = self
            .facts
            .poison
            .compare_exchange(0, errno, Ordering::AcqRel, Ordering::Acquire);
        OriginalBackingError::Unavailable(self.facts.poison.load(Ordering::Acquire))
    }

    fn lock_windows(&self) -> Result<MutexGuard<'_, WindowState>, OriginalBackingError> {
        self.windows.lock().map_err(|_| self.fail(libc::EIO))
    }

    fn record_managed(&self, windows: &WindowState) {
        let bytes = windows.managed_bytes(self.window_slots, self.page_size);
        self.facts.managed_bytes.store(bytes, Ordering::Release);
        self.facts
            .peak_managed_bytes
            .fetch_max(bytes, Ordering::AcqRel);
    }

    pub(crate) fn lease(
        &self,
        range: Range<usize>,
    ) -> Result<OriginalReadLease<'_>, OriginalBackingError> {
        self.health()?;
        if range.start > range.end || range.end > self.length || range.len() > ORIGINAL_READ_BYTES {
            return Err(OriginalBackingError::RangeOutsideMapping); // Validate the complete original range before any byte borrow.
        }
        let pins = {
            let mut windows = self.lock_windows()?;
            self.health()?;
            if self.facts.active_leases.load(Ordering::Acquire) >= MAX_ACTIVE_LEASES {
                return Err(OriginalBackingError::LeaseLimitExceeded);
            }
            let mut pages = [0; 2];
            let count = if range.is_empty() {
                0 // An empty read still owns a healthy lease, but pins no document page.
            } else {
                pages[0] = range.start / self.page_size * self.page_size;
                pages[1] = (range.end - 1) / self.page_size * self.page_size;
                if pages[0] == pages[1] { 1 } else { 2 }
            };
            let pins = windows
                .plan(&pages[..count], self.window_slots)
                .ok_or(OriginalBackingError::LeaseLimitExceeded)?;
            // Evict only after admitting the complete request. Candidates are unpinned
            // and outside this request; failure neither grows the budget nor waits
            // for another visitor while holding the directory lock.
            for at in 0..pins.count {
                let id = pins.slots[at];
                if windows.slots[id].page != Some(pins.pages[at])
                    && let Some(old) = windows.slots[id].page
                {
                    self.reclaim_page(old)?;
                    windows.slots[id].page = None;
                    self.record_managed(&windows);
                }
            }
            windows.pin(pins);
            self.record_managed(&windows);
            self.facts.active_leases.fetch_add(1, Ordering::AcqRel);
            pins
        };
        // SAFETY: The complete range lies in the immutable anonymous extent. Both the
        // active pins and owner borrow cover this slice; no full mapping is exposed.
        let bytes = unsafe {
            std::slice::from_raw_parts(self.address.as_ptr().add(range.start), range.len())
        };
        let lease = OriginalReadLease {
            owner: self,
            bytes,
            pins,
            finished: false,
        };
        // Recheck a concurrent failure after admission. On failure, the local lease
        // returns its actual pins before any slice reaches the caller.
        self.health()?;
        Ok(lease)
    }

    pub(crate) fn with_bytes<R>(
        &self,
        range: Range<usize>,
        visit: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, OriginalBackingError> {
        let lease = self.lease(range)?;
        let result = visit(&lease); // No directory lock crosses the visitor; bounded recursion remains possible.
        lease.finish()?; // Return pins and propagate required cleanup failures before publishing the result.
        Ok(result)
    }

    fn reclaim_page(&self, start: usize) -> Result<(), OriginalBackingError> {
        let length = self.page_size.min(self.length - start);
        bump(&self.facts.reclaim_attempts);
        // SAFETY: `start` is a system-page boundary inside this mapping and the final
        // length stays within its extent. Only an unpinned page or exclusive final
        // cleanup reaches this call.
        let result = unsafe {
            libc::msync(
                self.address.as_ptr().add(start).cast(),
                length,
                libc::MS_SYNC | libc::MS_INVALIDATE,
            )
        };
        if result != 0 {
            return Err(self.fail(last_errno()));
        }
        bump(&self.facts.reclaim_successes);
        #[cfg(any(test, feature = "original-backing-reclaim-probe"))]
        if self.facts.fail_next_reclaim.swap(false, Ordering::AcqRel) {
            return Err(self.fail(libc::EIO)); // Inject only after the real eviction succeeds; never invent a physical operation.
        }
        Ok(())
    }

    fn unpin(&self, pins: LeasePins) -> Result<(), OriginalBackingError> {
        let mut windows = match self.windows.lock() {
            Ok(windows) => windows,
            Err(poisoned) => {
                self.fail(libc::EIO);
                poisoned.into_inner() // Recover only to return the original claim; never clear poison or restore reads.
            }
        };
        for at in 0..pins.count {
            let slot = &mut windows.slots[pins.slots[at]];
            if slot.page == Some(pins.pages[at]) && slot.pins != 0 {
                slot.pins -= 1;
            } else {
                self.fail(libc::EINVAL); // An unbalanced claim forbids success; still compensate the remaining original pins.
            }
        }
        if self
            .facts
            .active_leases
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .is_err()
        {
            self.fail(libc::EINVAL);
        }
        self.health()
    }

    /// Document-feature tests explicitly evict unpinned pages to verify real cleanup
    /// failure and the first error observed by subsequent reads.
    #[cfg(all(test, feature = "native-markdown-candidate"))]
    pub(crate) fn reclaim_unpinned_for_test(&self) -> Result<(), OriginalBackingError> {
        let mut windows = self.lock_windows()?;
        self.health()?;
        for at in 0..self.window_slots {
            if windows.slots[at].pins == 0
                && let Some(page) = windows.slots[at].page
            {
                self.reclaim_page(page)?;
                windows.slots[at].page = None;
                self.record_managed(&windows);
            }
        }
        self.record_managed(&windows);
        self.health()
    }

    /// Consumes the exclusive owner to return managed slots and the complete VM extent.
    /// Shared documents and frozen text reach this path only through the final `Arc`.
    pub(crate) fn close(mut self) -> Result<(), OriginalBackingError> {
        self.release()
    }

    fn release(&mut self) -> Result<(), OriginalBackingError> {
        if !self.mapped {
            return self.health();
        } // Release this address only once; never retry after another mapping can reuse it.
        if self.facts.active_leases.load(Ordering::Acquire) != 0 {
            self.fail(libc::EBUSY);
        }
        {
            let mut windows = match self.windows.lock() {
                Ok(windows) => windows,
                Err(poisoned) => {
                    self.fail(libc::EIO);
                    poisoned.into_inner()
                }
            };
            for at in 0..self.window_slots {
                // Exclusive final cleanup attempts every acquired slot even after poison.
                // Later successes cannot replace the original failure.
                if let Some(page) = windows.slots[at].page
                    && self.reclaim_page(page).is_ok()
                {
                    windows.slots[at].page = None;
                }
            }
            self.record_managed(&windows);
        }
        self.mapped = false;
        self.facts.unmap_attempted.store(true, Ordering::Release);
        // SAFETY: The exclusive final owner has no live Rust borrows. The `mapped`
        // marker prevents a second `munmap` of an address that could be reused.
        let result = unsafe { libc::munmap(self.address.as_ptr().cast(), self.length) };
        if result == 0 {
            self.facts.mapped_bytes.store(0, Ordering::Release);
            self.facts.managed_bytes.store(0, Ordering::Release);
            self.facts.unmap_succeeded.store(true, Ordering::Release);
            self.health()
        } else {
            Err(self.fail(last_errno()))
        }
    }
}

/// Authorizes an original-byte slice only while its lease is live. Ending a pin
/// neither destroys the address nor retains a document or independent text copy.
pub(crate) struct OriginalReadLease<'a> {
    owner: &'a OriginalBacking,
    bytes: &'a [u8],
    pins: LeasePins,
    finished: bool,
}
impl Deref for OriginalReadLease<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.bytes
    }
}
impl OriginalReadLease<'_> {
    pub(crate) fn finish(mut self) -> Result<(), OriginalBackingError> {
        self.finished = true; // Return the original claim once; `Drop` must not decrement it again on failure.
        self.owner.unpin(self.pins)
    }
}
impl Drop for OriginalReadLease<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.owner.unpin(self.pins);
        }
    }
}

fn bump(value: &AtomicUsize) {
    let _ = value.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_add(1))
    });
}

impl Drop for OriginalBacking {
    fn drop(&mut self) {
        // `Drop` cannot return cleanup errors. Fixed observer facts retain the actual
        // kernel result; merely reaching this function never proves release.
        let _ = self.release();
    }
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(bytes: &[u8]) -> OriginalBacking {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        // SAFETY: This exclusive anonymous fixture has no writers or external handles
        // after the call; the validated extent exactly matches the initial bytes.
        unsafe { OriginalBacking::map(file, bytes.len()) }.unwrap()
    }

    #[test]
    fn actual_finite_lease_close_and_observer_release() {
        let source = vec![0x71; ORIGINAL_READ_BYTES * 3 + 1];
        let backing = fixture(&source);
        let observer = backing.observer();
        assert_eq!(observer.mapped_bytes(), source.len());
        for start in (0..source.len()).step_by(ORIGINAL_READ_BYTES) {
            let end = (start + ORIGINAL_READ_BYTES).min(source.len());
            backing
                .with_bytes(start..end, |bytes| assert_eq!(bytes, &source[start..end]))
                .unwrap();
        }
        assert_eq!(backing.facts.active_leases.load(Ordering::Acquire), 0);
        assert!(!observer.unmap_succeeded());
        backing.close().unwrap();
        assert!(observer.unmap_succeeded());
        assert_eq!(observer.mapped_bytes(), 0);
        assert!(!observer.failed());
    }

    #[test]
    fn callback_failure_never_masks_reclaim_failure_or_restores_health() {
        let backing = fixture(&vec![b'x'; MANAGED_WINDOW_BYTES * 2]);
        let observer = backing.observer();
        // Fill the real directory first; the visitor's new page must then perform
        // an actual eviction rather than satisfying the request from an empty slot.
        for at in 0..backing.window_slots {
            backing
                .with_bytes(at * backing.page_size..at * backing.page_size + 1, |_| ())
                .unwrap();
        }
        backing.fail_next_reclaim_for_test();
        let held = if backing.window_slots > 1 { 0..1 } else { 0..0 };
        let result = backing.with_bytes(held, |_| {
            assert_eq!(
                backing.with_bytes(MANAGED_WINDOW_BYTES..MANAGED_WINDOW_BYTES + 1, |_| ()),
                Err(OriginalBackingError::Unavailable(libc::EIO))
            );
            Err::<(), _>("callback stopped")
        });
        assert_eq!(result, Err(OriginalBackingError::Unavailable(libc::EIO)));
        assert_eq!(
            backing.with_bytes(0..1, |_| ()),
            Err(OriginalBackingError::Unavailable(libc::EIO))
        );
        assert_eq!(
            backing.close(),
            Err(OriginalBackingError::Unavailable(libc::EIO))
        );
        // Actual VM release and an earlier operation failure are independent facts;
        // successful final unmap must not rewrite the earlier failure.
        assert!(observer.unmap_succeeded());
        assert!(observer.failed());
        assert_eq!(observer.mapped_bytes(), 0);
    }

    #[test]
    fn nested_window_limit_rejects_before_read_and_releases_every_true_claim() {
        let backing = fixture(b"x");
        let mut leases = Vec::new();
        for _ in 0..MAX_ACTIVE_LEASES {
            leases.push(backing.lease(0..1).unwrap());
        }
        assert!(matches!(
            backing.lease(0..1),
            Err(OriginalBackingError::LeaseLimitExceeded)
        ));
        drop(leases);
        assert_eq!(backing.facts.active_leases.load(Ordering::Acquire), 0);
        backing
            .with_bytes(0..1, |bytes| assert_eq!(bytes, b"x"))
            .unwrap();
        backing.close().unwrap();
    }

    #[test]
    fn invalid_extent_is_rejected_without_touch_and_panicking_visitor_unpins_before_close() {
        let backing = fixture(&vec![0; ORIGINAL_READ_BYTES + 1]);
        assert!(matches!(
            backing.lease(0..ORIGINAL_READ_BYTES + 1),
            Err(OriginalBackingError::RangeOutsideMapping)
        ));
        assert!(matches!(
            backing.lease(std::ops::Range { start: 1, end: 0 }),
            Err(OriginalBackingError::RangeOutsideMapping)
        ));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = backing.with_bytes(0..1, |_| panic!("synthetic callback"));
        }));
        assert!(result.is_err());
        assert_eq!(backing.facts.active_leases.load(Ordering::Acquire), 0);
        let observer = backing.observer();
        assert_eq!(observer.reclaim_attempts(), 0); // Panic returns the pin; actual reclamation occurs on final close.
        backing.close().unwrap();
        assert_eq!(observer.reclaim_attempts(), 1);
        assert!(observer.unmap_succeeded());
    }
}

#[cfg(test)]
#[path = "original_backing/window_tests.rs"]
mod window_tests;
