//! Anonymous paged I/O for fixed-width records uses one 16 KiB append buffer.
//! Grapheme, line-break, and document semantics remain with their algorithms.
use std::{
    fs::File,
    os::fd::IntoRawFd,
    os::unix::fs::FileExt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// Each independent algorithm index owns one 16 KiB workspace and its own
/// backing file when needed, with no global or cross-algorithm cache.
pub const MARKDOWN_LAYOUT_INDEX_BYTES: usize = 16 * 1024;
const PAGE: usize = MARKDOWN_LAYOUT_INDEX_BYTES;
use std::io;

#[cfg(feature = "paged-line-compose")]
mod reader;
#[cfg(feature = "paged-line-compose")]
pub(crate) use reader::LayoutIndexReader;
#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) use prefix::LayoutIndexSnapshotError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LayoutIndexError {
    #[error("布局索引record或坐标无效")]
    InvalidRecord,
    #[error("布局索引坐标或计数溢出")]
    Overflow,
    #[error("布局索引已永久撤权")]
    Poisoned,
    #[error("布局索引IO失败")]
    Io(#[from] io::Error),
}
type Error = LayoutIndexError;
type Result<T> = std::result::Result<T, Error>;

#[derive(Default)]
struct ResourceState {
    #[cfg(feature = "paged-shaping-frontier")]
    producer_failed: AtomicBool,
    #[cfg(feature = "paged-shaping-frontier")]
    prefix_readers: AtomicUsize,
    file_open: AtomicBool,
    buffer: AtomicUsize,
    peak_buffer: AtomicUsize,
    #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
    owner_closed: AtomicBool,
    // Actual write/descriptor-return failure stays visible after later close;
    // this sticky result does not imply that every kernel close failed.
    cleanup_failed: AtomicBool,
    #[cfg(feature = "paged-line-compose")]
    reader_failed: AtomicBool,
}

/// One append page remains in RAM. Bytes below flushed already belong to the
/// same inode and are never overwritten after reader publication.
struct Page<const N: usize> {
    bytes: Option<Box<[u8; PAGE]>>,
    used: usize,
    flushed: usize,
    resources: Arc<ResourceState>,
}
impl<const N: usize> Page<N> {
    /// Allocate the sole page and publish its actual owned capacity receipt.
    fn new(resources: Arc<ResourceState>) -> Self {
        let bytes = Box::new([0; PAGE]);
        resources.buffer.store(PAGE, Ordering::Release);
        resources.peak_buffer.fetch_max(PAGE, Ordering::AcqRel);
        Self {
            bytes: Some(bytes),
            used: 0,
            flushed: 0,
            resources,
        }
    }

    /// Return exactly one initialized record from the original append page.
    fn read(&self, offset: usize) -> Result<[u8; N]> {
        let end = offset
            .checked_add(N)
            .filter(|&end| end <= self.used)
            .ok_or(Error::InvalidRecord)?;
        let mut record = [0; N];
        record.copy_from_slice(&self.bytes.as_ref().ok_or(Error::Poisoned)?[offset..end]);
        Ok(record)
    }

    /// Append one complete record at the current initialized boundary only.
    fn write(&mut self, offset: usize, record: [u8; N]) -> Result<()> {
        let end = offset
            .checked_add(N)
            .filter(|&end| end <= PAGE)
            .ok_or(Error::InvalidRecord)?;
        if offset != self.used {
            // Previously published bytes remain immutable, and gaps cannot
            // become apparently initialized records.
            return Err(Error::InvalidRecord);
        }
        self.bytes.as_mut().ok_or(Error::Poisoned)?[offset..end].copy_from_slice(&record);
        self.used = end;
        Ok(())
    }

    /// Write only the new dirty suffix. A published half-page prefix stays
    /// byte-for-byte immutable for every already-issued reader.
    fn flush_through(&mut self, file: &File, page_start: usize, through: usize) -> Result<()> {
        let through = through.min(self.used);
        if through <= self.flushed {
            // The requested prefix already exists on the original inode.
            return Ok(());
        }
        let offset = page_start
            .checked_add(self.flushed)
            .ok_or(Error::Overflow)?;
        let offset = u64::try_from(offset).map_err(|_| Error::Overflow)?;
        let bytes = self.bytes.as_ref().ok_or(Error::Poisoned)?;
        // write_all_at handles short writes/EINTR within this finite suffix.
        // A later failure invalidates the candidate despite any written prefix.
        if let Err(error) = file.write_all_at(&bytes[self.flushed..through], offset) {
            self.resources.cleanup_failed.store(true, Ordering::Release);
            return Err(error.into());
        }
        self.flushed = through;
        Ok(())
    }

    /// Reuse the same allocation after full old-page flush. Previous bytes do
    /// not authorize initialized records in the new logical page.
    fn reset(&mut self) {
        self.used = 0;
        self.flushed = 0;
    }
}
impl<const N: usize> Drop for Page<N> {
    fn drop(&mut self) {
        // Unpublished tail bytes have no consumer. Drop the actual unique
        // allocation before publishing a zero-capacity receipt.
        drop(self.bytes.take());
        self.resources.buffer.store(0, Ordering::Release);
    }
}

/// Read one fixed extent with the same short-read/EINTR protocol for builders
/// and readers, without adding a mapping or shared file cursor.
fn read_exact_at(
    output: &mut [u8],
    offset: u64,
    mut read: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
) -> Result<()> {
    if output.len() > PAGE {
        return Err(Error::InvalidRecord);
    } // This shared entry keeps the actual 16 KiB bound; callers cannot turn
    // it into a complete-index materialization path.
    offset
        .checked_add(u64::try_from(output.len()).map_err(|_| Error::Overflow)?)
        .ok_or(Error::Overflow)?;
    let mut complete = 0usize;
    while complete < output.len() {
        let position = offset
            .checked_add(u64::try_from(complete).map_err(|_| Error::Overflow)?)
            .ok_or(Error::Overflow)?;
        match read(&mut output[complete..], position) {
            Ok(0) => {
                // A filled stack prefix is incomplete. Preserve the real EOF
                // failure instead of publishing a partial record.
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "layout index ended before the requested records",
                )
                .into());
            }
            Ok(bytes) => {
                // Advance only over successfully read bytes of this same extent.
                complete = complete.checked_add(bytes).ok_or(Error::Overflow)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                // EINTR retries the original unfinished extent without
                // rebuilding the candidate, page, or reading authority.
            }
            Err(error) => return Err(error.into()), // The actual first errno
                                                    // reaches the owner's sticky rejection.
        }
    }
    Ok(())
}

/// Settle the unique descriptor once through the same close boundary, without
/// duplicating File ownership for read-only handoff or guessing a retry.
fn close_file(file: File, resources: &ResourceState) -> Result<()> {
    let raw = file.into_raw_fd();
    // SAFETY: into_raw_fd transfers the sole close responsibility; no second
    // File owns this descriptor and can close a reused number later.
    if unsafe { libc::close(raw) } == 0 {
        resources.file_open.store(false, Ordering::Release);
        Ok(())
    } else {
        // A failed close grants no successful-return receipt and must not be
        // retried against a descriptor number the system may have reused.
        resources.cleanup_failed.store(true, Ordering::Release);
        Err(io::Error::last_os_error().into())
    }
}

/// Append-only algorithm metadata owns one active page and, only when required,
/// one anonymous file. Sealing and resource release remain separate operations.
pub(crate) struct PagedLayoutIndex<const N: usize> {
    page: Option<Page<N>>,
    file: Option<File>,
    number: usize,
    count: usize,
    loads: u64,
    sealed: bool,
    poisoned: bool,
    resources: Arc<ResourceState>,
}
impl<const N: usize> PagedLayoutIndex<N> {
    pub(crate) fn new() -> Result<Self> {
        // Records fit wholly within the fixed workspace; this backend requires
        // neither VM-page alignment nor eager disk allocation.
        if N == 0 || N > PAGE || !PAGE.is_multiple_of(N) {
            return Err(Error::InvalidRecord);
        }
        let resources = Arc::new(ResourceState::default());
        Ok(Self {
            page: None,
            file: None, // A private single page uses its unique buffer. Create
            // no anonymous FD until an actual file consumer exists.
            number: 0,
            count: 0,
            loads: 0,
            sealed: false,
            poisoned: false,
            resources,
        })
    }
    #[cfg(all(test, any(feature = "paged-graphemes", feature = "paged-line-breaks")))]
    /// Resource-boundary tests supply a real descriptor to the same index owner.
    pub(crate) fn test_with_file(file: File) -> Result<Self> {
        Self::with_file(file)
    }
    #[cfg(test)]
    fn with_file(file: File) -> Result<Self> {
        let mut index = Self::new()?;
        // Actual read-only, directory, or truncatable descriptors still use
        // production write/read/close operations after this resource setup.
        index.file = Some(file);
        index.resources.file_open.store(true, Ordering::Release);
        Ok(index)
    }

    /// Local rejection and original write/close failure permanently end access.
    fn healthy(&self) -> Result<()> {
        if self.poisoned
            || self.resources.cleanup_failed.load(Ordering::Acquire)
            || self.producer_has_failed()
        {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Allocate a real file only for page eviction or reader handoff. The
    /// calling operation permanently rejects the original owner on failure.
    fn ensure_file(&mut self) -> Result<()> {
        if self.file.is_none() {
            // Return creation failure at its actual I/O boundary. Failure to
            // obtain a descriptor does not fabricate a close failure or retry.
            let file = tempfile::tempfile()?;
            self.file = Some(file);
            self.resources.file_open.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Prefix readers observe the same producer failure when that feature owns
    /// published prefixes; other builds have no such upstream resource.
    fn producer_has_failed(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.resources.producer_failed.load(Ordering::Acquire)
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }

    /// Revoke the original writer and every feature-enabled prefix observer.
    fn mark_failed(&mut self) {
        self.poisoned = true;
        #[cfg(feature = "paged-shaping-frontier")]
        self.resources
            .producer_failed
            .store(true, Ordering::Release);
    }

    /// Locate the next append record, flushing an old page completely before
    /// reusing its sole allocation for another logical page.
    fn page(&mut self, record: usize) -> Result<usize> {
        self.healthy()?;
        let result = (|| {
            let offset = record.checked_mul(N).ok_or(Error::Overflow)?;
            let number = offset / PAGE;
            if self.page.is_none() {
                // First use creates the fixed buffer, with no speculative file.
                self.page = Some(Page::new(Arc::clone(&self.resources)));
                self.number = number;
                self.loads = self.loads.checked_add(1).ok_or(Error::Overflow)?;
            } else if self.number != number {
                // The old page is leaving RAM. Complete its real file write
                // before resetting initialized coverage in the reused buffer.
                let start = self.number.checked_mul(PAGE).ok_or(Error::Overflow)?;
                self.ensure_file()?;
                let file = self.file.as_ref().ok_or(Error::Poisoned)?;
                let page = self.page.as_mut().ok_or(Error::Poisoned)?;
                page.flush_through(file, start, PAGE)?;
                page.reset();
                self.number = number;
                self.loads = self.loads.checked_add(1).ok_or(Error::Overflow)?;
            }
            Ok(offset % PAGE)
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }

    /// Flush only for file-backed reading authority or eviction; a private seal
    /// continues to read the same already-owned append buffer.
    #[cfg(feature = "paged-line-compose")]
    fn flush_through(&mut self, records: usize) -> Result<()> {
        self.healthy()?;
        let result = (|| {
            let end = records.checked_mul(N).ok_or(Error::Overflow)?;
            if records > self.count {
                return Err(Error::InvalidRecord);
            }
            // Even empty into_reader hands off one actual file, never a missing
            // descriptor or another buffer pretending to be a reader.
            self.ensure_file()?;
            if let Some(page) = self.page.as_mut() {
                let start = self.number.checked_mul(PAGE).ok_or(Error::Overflow)?;
                if end > start {
                    page.flush_through(
                        self.file.as_ref().ok_or(Error::Poisoned)?,
                        start,
                        end - start,
                    )?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }

    /// Publish count only after one complete record is appended successfully.
    pub(crate) fn append(&mut self, record: [u8; N]) -> Result<()> {
        self.healthy()?;
        if self.sealed {
            // Sealing ends append authority permanently, including later reads
            // through an owner that attempted to violate that boundary.
            self.mark_failed();
            return Err(Error::Poisoned);
        }
        let result: Result<()> = (|| {
            let offset = self.page(self.count)?;
            self.page
                .as_mut()
                .ok_or(Error::InvalidRecord)?
                .write(offset, record)?;
            self.count = self.count.checked_add(1).ok_or(Error::Overflow)?;
            Ok(())
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }
    /// Read a complete existing record through the sole buffer or original FD.
    pub(crate) fn read(&mut self, record: usize) -> Result<[u8; N]> {
        self.healthy()?;
        if record >= self.count {
            // Capacity or file extent cannot stand in for initialized count.
            self.mark_failed();
            return Err(Error::InvalidRecord);
        }
        let result = (|| {
            let offset = record.checked_mul(N).ok_or(Error::Overflow)?;
            if let Some(page) = self.page.as_ref()
                && self.number == offset / PAGE
            {
                // An append-page hit reads the existing allocation directly;
                // sealing does not force a write followed by a redundant read.
                return page.read(offset % PAGE);
            }
            let file = self.file.as_ref().ok_or(Error::Poisoned)?;
            let offset = u64::try_from(offset).map_err(|_| Error::Overflow)?;
            let mut bytes = [0; N];
            // Old pages were flushed during eviction. Read one stack record
            // from the original FD without prefetching or evicting the buffer.
            read_exact_at(&mut bytes, offset, |output, position| {
                file.read_at(output, position)
            })?;
            Ok(bytes)
        })();
        if result.is_err() {
            self.mark_failed();
        }
        result
    }

    /// Deliver one explicitly requested small contiguous batch, without reading
    /// beyond its extent or allocating a second page cache.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn read_batch(
        &mut self,
        range: std::ops::Range<usize>,
        output: &mut [[u8; N]],
    ) -> Result<()> {
        self.healthy()?;
        let result = (|| {
            let records = range
                .end
                .checked_sub(range.start)
                .filter(|&records| records == output.len() && records <= 256)
                .ok_or(Error::InvalidRecord)?;
            let length = records.checked_mul(N).ok_or(Error::Overflow)?;
            if range.end > self.count || length > PAGE {
                return Err(Error::InvalidRecord);
            } // Reject a mismatched range/output budget as a whole; shortening
            // it cannot authorize a partial successful batch.
            if records == 0 {
                return Ok(()); // Empty output requires neither a file nor an
                // append page that has not been allocated.
            }
            let start = range.start.checked_mul(N).ok_or(Error::Overflow)?;
            let end = range.end.checked_mul(N).ok_or(Error::Overflow)?;
            let append_start = self.number.checked_mul(PAGE).ok_or(Error::Overflow)?;
            let output = output.as_flattened_mut();
            let file_end = end.min(append_start);
            let file_length = file_end.saturating_sub(start);
            if file_length != 0 {
                // Previously flushed records go directly into this bounded
                // output interval; the rest of their page is never read.
                let file = self.file.as_ref().ok_or(Error::Poisoned)?;
                let offset = u64::try_from(start).map_err(|_| Error::Overflow)?;
                read_exact_at(&mut output[..file_length], offset, |bytes, position| {
                    file.read_at(bytes, position)
                })?;
            }
            if file_length < length {
                // The crossing suffix remains in the unique append buffer.
                // Its unflushed records cannot be read as zeros from the FD.
                let page = self.page.as_ref().ok_or(Error::Poisoned)?;
                let local_start = start.max(append_start) - append_start;
                let local_end = end.checked_sub(append_start).ok_or(Error::InvalidRecord)?;
                if local_end > page.used {
                    // Only fully initialized append bytes can fill the suffix.
                    return Err(Error::InvalidRecord);
                }
                let bytes = page.bytes.as_ref().ok_or(Error::Poisoned)?;
                output[file_length..].copy_from_slice(&bytes[local_start..local_end]);
            }
            Ok(())
        })();
        if result.is_err() {
            self.mark_failed(); // Partial I/O and invalid ranges both revoke
            // the owner; no already-read prefix is published.
        }
        result
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// Describe initialized records of the unique append buffer without loading
    /// an old page or creating another read cache.
    #[cfg(any(feature = "paged-graphemes", feature = "paged-line-breaks"))]
    pub(crate) fn append_page_records(&mut self) -> Result<Option<std::ops::Range<usize>>> {
        self.healthy()?;
        let result = (|| {
            let Some(page) = &self.page else {
                return Ok(None); // An absent page grants no initialized zero
                // records or permission to read capacity.
            };
            let start = self.number.checked_mul(PAGE / N).ok_or(Error::Overflow)?;
            let end = start.checked_add(page.used / N).ok_or(Error::Overflow)?;
            // page.used advances only after a complete record write; seal keeps
            // that initialized extent in the same buffer. Neither capacity nor
            // flushed byte count certifies unwritten records in a partial page.
            Ok(Some(start..end))
        })();
        if result.is_err() {
            self.mark_failed(); // Coordinate failure still rejects the original
            // owner; a query optimization cannot recreate it.
        }
        result
    }

    #[cfg(any(feature = "paged-graphemes", feature = "paged-line-breaks"))]
    /// Observe actual workspace-page activations without loading a page.
    pub(crate) fn page_loads(&self) -> u64 {
        self.loads
    }
    #[cfg(any(feature = "paged-graphemes", feature = "paged-line-breaks"))]
    /// The owning algorithm uses this terminal path after its real failure.
    pub(crate) fn poison(&mut self) {
        self.mark_failed();
    }
    /// Share resource facts without retaining the page or descriptor itself.
    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        LayoutIndexObserver {
            own: Arc::clone(&self.resources),
            #[cfg(feature = "paged-shaping-frontier")]
            upstream: None,
        }
    }
    pub(crate) fn seal(&mut self) -> Result<()> {
        self.healthy()?;
        // Seal private append permission only. Same-owner reads retain the
        // buffer without a forced write for a nonexistent file consumer.
        self.sealed = true;
        Ok(())
    }

    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn into_reader(mut self) -> Result<LayoutIndexReader<N>> {
        // Handoff requires every logical record on the actual file. On failure
        // the original owner returns its buffer/FD without a partial reader.
        self.seal()?;
        self.flush_through(self.count)?;
        drop(self.page.take());
        self.count
            .checked_mul(N)
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(Error::Overflow)?;
        if self.resources.buffer.load(Ordering::Acquire) != 0 {
            // A reader owns the original file and receipts only; a surviving
            // writer allocation cannot be handed to a background reader.
            self.mark_failed();
            return Err(Error::Poisoned);
        }
        let file = self.file.take().ok_or(Error::Poisoned)?;
        // Move the unique original file. This value's Drop has no buffer/FD
        // left to close; observers still refer to the same resource identity.
        Ok(LayoutIndexReader::from_sealed(
            file,
            self.count,
            Arc::clone(&self.resources),
        ))
    }

    pub(crate) fn close(&mut self) -> Result<()> {
        let mut first = None;
        // Close issues no reader. Discard unpublished dirty tail bytes without
        // writing temporary data that has no consumer.
        drop(self.page.take());
        if let Some(file) = self.file.take()
            && let Err(error) = close_file(file, &self.resources)
            && first.is_none()
        {
            first = Some(error);
        }
        self.poisoned = true; // file=None can mean never allocated or already
        // returned; this terminal state ends future access.
        #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
        self.resources.owner_closed.store(true, Ordering::Release); // Mark the
        // writer ended after return/handoff; an issued reader owns its FD alone.
        if let Some(error) = first {
            #[cfg(feature = "paged-shaping-frontier")]
            self.resources
                .producer_failed
                .store(true, Ordering::Release);
            Err(error)
        } else if self.resources.cleanup_failed.load(Ordering::Acquire) {
            // Returning all presently owned resources cannot erase an earlier
            // write/return failure or reclassify that operation as successful.
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
}
impl<const N: usize> Drop for PagedLayoutIndex<N> {
    fn drop(&mut self) {
        if self.close().is_err() {
            self.resources.cleanup_failed.store(true, Ordering::Release);
        }
    }
}

/// Atomic resource facts retain no FD, mapping, Source, algorithm, or Native
/// object and make no claim about current kernel RSS.
#[derive(Clone)]
pub(crate) struct LayoutIndexObserver {
    own: Arc<ResourceState>,
    #[cfg(feature = "paged-shaping-frontier")]
    upstream: Option<Arc<ResourceState>>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayoutIndexResources {
    pub(crate) file_open: bool,
    /// Issued prefix FDs without successful close, including unknown outcomes.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) prefix_readers: usize,
    /// Actual write/prefix-read/return failure of the original inode, shared by
    /// every subsequent prefix observer.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) producer_failed: bool,
    /// The writer closed or handed off its File; an issued reader may still own it.
    #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
    pub(crate) owner_closed: bool,
    /// This metadata backend never maps; its actual zero remains distinct from
    /// Source/shaper mapping receipts.
    pub(crate) mapped_bytes: usize,
    pub(crate) peak_mapped_bytes: usize,
    /// Actual current/peak allocation of the unique Box, with no reader buffer
    /// or copied prefix buffer included.
    pub(crate) buffer_bytes: usize,
    pub(crate) peak_buffer_bytes: usize,
    pub(crate) cleanup_failed: bool,
    #[cfg(feature = "paged-line-compose")]
    pub(crate) reader_failed: bool,
}
impl LayoutIndexObserver {
    /// Report each original resource outcome independently; zero metadata
    /// mappings do not prove that other Source/font mappings or RSS are zero.
    pub(crate) fn snapshot(&self) -> LayoutIndexResources {
        LayoutIndexResources {
            file_open: self.own.file_open.load(Ordering::Acquire),
            #[cfg(feature = "paged-shaping-frontier")]
            prefix_readers: self.own.prefix_readers.load(Ordering::Acquire),
            #[cfg(feature = "paged-shaping-frontier")]
            producer_failed: self.own.producer_failed.load(Ordering::Acquire),
            #[cfg(any(feature = "paged-graphemes", feature = "paged-shaping"))]
            owner_closed: self.own.owner_closed.load(Ordering::Acquire),
            mapped_bytes: 0,
            peak_mapped_bytes: 0,
            buffer_bytes: self.own.buffer.load(Ordering::Acquire),
            peak_buffer_bytes: self.own.peak_buffer.load(Ordering::Acquire),
            cleanup_failed: self.own.cleanup_failed.load(Ordering::Acquire),
            #[cfg(feature = "paged-line-compose")]
            reader_failed: self.own.reader_failed.load(Ordering::Acquire)
                || self.upstream_has_failed(),
        }
    }

    #[cfg(feature = "paged-line-compose")]
    fn upstream_has_failed(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.upstream.as_ref().is_some_and(|upstream| {
                upstream.producer_failed.load(Ordering::Acquire)
                    || upstream.cleanup_failed.load(Ordering::Acquire)
            })
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }
}

#[cfg(test)]
#[path = "markdown_layout_index/read_tests.rs"]
mod read_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_widths_round_trip_across_pages_reusing_one_actual_buffer() {
        fn check<const N: usize>() {
            let mut index = PagedLayoutIndex::<N>::new().unwrap();
            let observer = index.resource_observer();
            // Each actual record encodes independent head/tail values across
            // several pages, exposing incorrect page reuse or positioned reads.
            for row in 0usize..2049 {
                let mut bytes = [0; N];
                bytes[..8].copy_from_slice(&(row as u64).to_le_bytes());
                bytes[N - 1] = (row % 251) as u8;
                index.append(bytes).unwrap();
                assert_eq!(observer.snapshot().mapped_bytes, 0);
                assert_eq!(observer.snapshot().buffer_bytes, PAGE);
            }
            index.seal().unwrap();
            // Nonsequential queries cross both active and evicted pages after
            // seal; expected bytes come from the original row coordinates.
            for row in [2048usize, 0, 513, 1024, 511, 2047] {
                let bytes = index.read(row).unwrap();
                assert_eq!(
                    u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                    row as u64
                );
                assert_eq!(bytes[N - 1], (row % 251) as u8);
                assert_eq!(observer.snapshot().mapped_bytes, 0);
                assert_eq!(observer.snapshot().buffer_bytes, PAGE);
            }
            index.close().unwrap();
            // Actual close/drop facts remain independent of the content checks
            // and of any kernel memory measurement.
            let state = observer.snapshot();
            assert!(
                !state.file_open
                    && state.mapped_bytes == 0
                    && state.buffer_bytes == 0
                    && !state.cleanup_failed
            );
            assert_eq!(state.peak_mapped_bytes, 0);
            assert_eq!(state.buffer_bytes, 0);
            assert_eq!(state.peak_buffer_bytes, PAGE);
        }
        check::<16>();
        check::<32>();
        check::<64>();
    }
    #[test]
    fn actual_readonly_write_failure_remains_negative_after_buffer_and_file_return() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly-index");
        std::fs::write(&path, b"unchanged").unwrap();
        let mut index = PagedLayoutIndex::<16>::with_file(File::open(&path).unwrap()).unwrap();
        let observer = index.resource_observer();
        // The private first page appends to its unique buffer. Crossing the
        // page boundary is the first actual write_all_at on this read-only FD.
        for _ in 0..PAGE / 16 {
            index.append([3; 16]).unwrap();
        }
        assert_eq!(index.read(0).unwrap(), [3; 16]);
        assert!(
            matches!(index.append([4; 16]), Err(Error::Io(error)) if error.raw_os_error() == Some(libc::EBADF))
        );
        assert!(matches!(index.read(0), Err(Error::Poisoned)));
        assert!(matches!(index.close(), Err(Error::Poisoned)));
        let state = observer.snapshot();
        assert!(
            !state.file_open
                && state.mapped_bytes == 0
                && state.buffer_bytes == 0
                && state.cleanup_failed
        );
        assert_eq!(state.peak_buffer_bytes, PAGE);
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
        assert!(matches!(index.close(), Err(Error::Poisoned)));
        drop(index);
        assert!(observer.snapshot().cleanup_failed);
    }
    #[test]
    fn invalid_width_and_post_seal_write_never_revive_cached_records() {
        // Record widths must divide the actual fixed workspace, including the
        // zero and oversized cases that cannot describe a complete record.
        assert!(matches!(
            PagedLayoutIndex::<0>::new(),
            Err(LayoutIndexError::InvalidRecord)
        ));
        assert!(matches!(
            PagedLayoutIndex::<3>::new(),
            Err(LayoutIndexError::InvalidRecord)
        ));
        assert!(matches!(
            PagedLayoutIndex::<32768>::new(),
            Err(LayoutIndexError::InvalidRecord)
        ));
        let mut index = PagedLayoutIndex::<32>::new().unwrap();
        let observer = index.resource_observer();
        index.append([7; 32]).unwrap();
        index.seal().unwrap();
        assert_eq!(index.read(0).unwrap(), [7; 32]);
        // A prohibited post-seal append permanently ends this writer's access;
        // previously cached bytes cannot revive it as a fresh valid owner.
        assert!(matches!(
            index.append([9; 32]),
            Err(LayoutIndexError::Poisoned)
        ));
        assert!(matches!(index.read(0), Err(LayoutIndexError::Poisoned)));
        drop(index);
        let state = observer.snapshot();
        assert!(
            !state.file_open
                && state.mapped_bytes == 0
                && state.buffer_bytes == 0
                && !state.cleanup_failed
        );
    }
}
