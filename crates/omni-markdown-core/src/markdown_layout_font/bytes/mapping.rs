//! Font-only read-only VM ownership. It grants no whole Markdown-body lease and never equates
//! completing a Drop attempt with successful resource return.
use std::{
    io,
    os::fd::RawFd,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    },
};

use crate::immutable_snapshot::FrozenFontFile;

/// Fixed atomic receipts retain no descriptor, address, mapping, or Font. They record actual syscall
/// outcomes after final disposal, without proving current kernel RSS or retaining resource ownership.
#[derive(Default)]
pub(super) struct MappingFacts {
    pub(super) completed: AtomicBool,
    pub(super) unmapped: AtomicBool,
    pub(super) mapped_bytes: AtomicUsize,
    pub(super) backing_closed: AtomicBool,
    pub(super) source_closed: AtomicBool,
    errno: AtomicI32,
}
impl MappingFacts {
    fn record_error(&self, error: &io::Error) {
        // Keep the first actual failure across cleanup. Errors without a nonzero OS errno still
        // need a negative receipt, so represent them by EIO rather than apparent success.
        let errno = error
            .raw_os_error()
            .filter(|errno| *errno != 0)
            .unwrap_or(libc::EIO);
        let _ = self
            .errno
            .compare_exchange(0, errno, Ordering::AcqRel, Ordering::Acquire);
    }
    pub(super) fn first_errno(&self) -> Option<i32> {
        match self.errno.load(Ordering::Acquire) {
            0 => None,
            errno => Some(errno),
        }
    }
    fn result(&self) -> io::Result<()> {
        self.first_errno()
            .map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
    }
}

/// Construct only from the dedicated factory's opaque frozen ticket, never an arbitrary mutable
/// File accepted by a safe caller-facing constructor.
pub(super) struct ReadonlyFontMapping {
    address: NonNull<u8>,
    length: usize,
    facts: Arc<MappingFacts>,
    attempted: bool,
}
// SAFETY: the factory finished writing its independent anonymous inode before mapping. Consuming
// its ticket leaves no other File handle. PROT_READ bytes remain immutable, and Rust lifetimes end
// all shared borrows before the final owner's Drop.
unsafe impl Send for ReadonlyFontMapping {}
// SAFETY: threads only read the same fixed extent; mutable receipts are atomic and unmapping
// requires exclusive disposal of the actual owner.
unsafe impl Sync for ReadonlyFontMapping {}

impl ReadonlyFontMapping {
    pub(super) fn map(file: FrozenFontFile) -> io::Result<Self> {
        Self::map_with_close(file, Arc::new(MappingFacts::default()), close_descriptor)
    }
    fn map_with_close(
        file: FrozenFontFile,
        facts: Arc<MappingFacts>,
        close: impl FnOnce(RawFd) -> io::Result<()>,
    ) -> io::Result<Self> {
        let length = file.len();
        let fd = file.into_raw_fd();
        // SAFETY: the verified frozen ticket transfers unique descriptor ownership with a nonempty
        // slice-compatible extent. A read-only shared mapping does not copy the complete font.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            let error = io::Error::last_os_error();
            facts.record_error(&error);
            // Even without VM, close the consumed unique descriptor; a later close error cannot
            // replace the original mmap failure.
            match close(fd) {
                Ok(()) => facts.backing_closed.store(true, Ordering::Release),
                Err(error) => facts.record_error(&error),
            }
            facts.completed.store(true, Ordering::Release);
            return Err(error);
        }
        facts.mapped_bytes.store(length, Ordering::Release);
        let Some(address) = NonNull::new(raw.cast::<u8>()) else {
            let error = io::Error::from_raw_os_error(libc::EINVAL);
            facts.record_error(&error);
            // SAFETY: no Rust reference was published for this unusual zero-address mapping;
            // record the actual compensating syscall outcomes separately.
            let status = unsafe { libc::munmap(raw, length) };
            record_unmap(&facts, status);
            match close(fd) {
                Ok(()) => facts.backing_closed.store(true, Ordering::Release),
                Err(error) => facts.record_error(&error),
            }
            facts.completed.store(true, Ordering::Release);
            return Err(error);
        };
        let mut mapping = Self {
            address,
            length,
            facts,
            attempted: false,
        };
        match close(fd) {
            Ok(()) => mapping.facts.backing_closed.store(true, Ordering::Release),
            Err(error) => {
                // A close failure prevents blob publication. Return the unborrowed VM once,
                // without retrying a descriptor number that may already have been reused.
                mapping.facts.record_error(&error);
                let _ = mapping.release();
                return Err(error);
            }
        }
        Ok(mapping)
    }
    pub(super) fn len(&self) -> usize {
        self.length
    }
    pub(super) fn facts(&self) -> Arc<MappingFacts> {
        Arc::clone(&self.facts)
    }
    pub(super) fn source_closed(&self) {
        self.facts.source_closed.store(true, Ordering::Release);
    }
    pub(super) fn record_error(&self, error: &io::Error) {
        self.facts.record_error(error);
    }
    pub(super) fn close(mut self) -> io::Result<()> {
        self.release()
    }
    fn release(&mut self) -> io::Result<()> {
        self.release_with(|address, length| {
            // SAFETY: exclusive Drop/consume ends all font borrows, and this address/length is the
            // actual mmap extent owned by this value.
            unsafe { libc::munmap(address.cast(), length) }
        })
    }
    fn release_with(&mut self, unmap: impl FnOnce(*mut u8, usize) -> i32) -> io::Result<()> {
        if self.attempted {
            return self.facts.result();
        }
        self.attempted = true; // Never reclaim/retry this address after an attempt, including failure, because it may be reused.
        record_unmap(&self.facts, unmap(self.address.as_ptr(), self.length));
        self.facts.completed.store(true, Ordering::Release);
        self.facts.result()
    }
}
impl AsRef<[u8]> for ReadonlyFontMapping {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: this slice borrows a live owner. The font factory guarantees immutable bytes and
        // a length no greater than isize::MAX; document-body leases use a different bounded owner.
        unsafe { std::slice::from_raw_parts(self.address.as_ptr(), self.length) }
    }
}
impl Drop for ReadonlyFontMapping {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
fn record_unmap(facts: &MappingFacts, status: i32) {
    if status == 0 {
        facts.mapped_bytes.store(0, Ordering::Release);
        facts.unmapped.store(true, Ordering::Release);
    } else {
        // Capture errno before another syscall. Without a successful unmap receipt, retain the
        // original extent instead of reporting a fabricated zero.
        facts.record_error(&io::Error::last_os_error());
    }
}
pub(super) fn close_descriptor(fd: RawFd) -> io::Result<()> {
    // SAFETY: into_raw_fd transferred this sole close responsibility; call once without blindly
    // retrying EINTR on a potentially reused descriptor.
    if unsafe { libc::close(fd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests;
