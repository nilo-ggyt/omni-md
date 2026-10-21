//! One anonymous metadata file and a fixed page cache shared by all tables; document text never enters this file.

use std::fs::File;
use std::os::fd::IntoRawFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use super::resource::ResourceState;
use super::{BidiError, BidiMemoryBudget, MarkdownBidiCancellation, MarkdownBidiResourceObserver};

pub(super) const PAGE_BYTES: usize = 16 * 1024;
pub(super) const RECORD_BYTES: u64 = 32;
pub(super) const NONE: u32 = u32::MAX;

#[derive(Clone, Copy)]
pub(super) enum Table {
    Text = 0,
    Run = 1,
    Sequence = 2,
    Stack = 3,
    Paragraph = 4,
}

struct Page {
    number: u64,
    bytes: PageBytes,
    dirty: bool,
    used: u64,
}

/// Drop the actual fixed Box before recording release, so Store field destruction cannot report a still-live page as returned.
struct PageBytes {
    bytes: Option<Box<[u8; PAGE_BYTES]>>,
    resources: Arc<ResourceState>,
}
impl PageBytes {
    fn new(resources: &Arc<ResourceState>) -> Self {
        let bytes = Box::new([0; PAGE_BYTES]);
        resources.page_allocated(PAGE_BYTES);
        Self {
            bytes: Some(bytes),
            resources: Arc::clone(resources),
        }
    }
}
impl std::ops::Deref for PageBytes {
    type Target = [u8; PAGE_BYTES];
    fn deref(&self) -> &Self::Target {
        self.bytes
            .as_deref()
            .expect("存活的双向文本页必须拥有原缓存")
    }
}
impl std::ops::DerefMut for PageBytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.bytes
            .as_deref_mut()
            .expect("存活的双向文本页必须拥有原缓存")
    }
}
impl Drop for PageBytes {
    fn drop(&mut self) {
        if let Some(bytes) = self.bytes.take() {
            drop(bytes);
            self.resources.page_deallocated(PAGE_BYTES);
        }
    }
}

/// Forward actual positioned I/O; existing test failures perform real prefix I/O first and do not compute substitute algorithm results.
struct Io {
    file: Option<File>,
    resources: Arc<ResourceState>,
    #[cfg(test)]
    fault: Option<IoFault>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum IoFault {
    PartialRead(usize),
    PartialWrite(usize),
    SetLength,
}

impl Io {
    #[cfg(test)]
    fn new(file: File) -> Self {
        Self::with_resources(file, ResourceState::new())
    }

    fn with_resources(file: File, resources: Arc<ResourceState>) -> Self {
        resources.file_acquired(); // Record an FD only after the caller has actually acquired File ownership for this scope.
        Self {
            file: Some(file),
            resources,
            #[cfg(test)]
            fault: None,
        }
    }

    fn file(&self) -> &File {
        self.file.as_ref().expect("live metadata IO owns one file")
    }

    #[cfg(test)]
    fn observer(&self) -> FileObserver {
        self.resources.observe()
    }

    fn set_len(&mut self, length: u64) -> std::io::Result<()> {
        self.file().set_len(length)?;
        #[cfg(test)]
        if matches!(self.fault, Some(IoFault::SetLength)) {
            self.fault = None;
            return Err(std::io::Error::other("synthetic metadata set_len failure"));
        }
        Ok(())
    }

    fn read_page(&mut self, destination: &mut [u8], offset: u64) -> std::io::Result<()> {
        #[cfg(test)]
        if let Some(IoFault::PartialRead(length)) = self.fault {
            self.fault = None;
            let length = length.min(destination.len());
            self.file()
                .read_exact_at(&mut destination[..length], offset)?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "synthetic metadata partial read",
            ));
        }
        self.file().read_exact_at(destination, offset)
    }

    fn write_page(&mut self, source: &[u8], offset: u64) -> std::io::Result<()> {
        #[cfg(test)]
        if let Some(IoFault::PartialWrite(length)) = self.fault {
            self.fault = None;
            self.file()
                .write_all_at(&source[..length.min(source.len())], offset)?;
            return Err(std::io::Error::other("synthetic metadata partial write"));
        }
        self.file().write_all_at(source, offset)
    }
}

impl Drop for Io {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let fd = file.into_raw_fd();
            // SAFETY: Move out the sole anonymous FD owner and close exactly once; no mapping or external borrow depends on this descriptor.
            let result = unsafe { libc::close(fd) };
            let status = if result == 0 {
                1
            } else {
                // Read errno immediately after actual close, before another syscall can replace it; do not guess that a failed close returned the FD.
                -std::io::Error::last_os_error()
                    .raw_os_error()
                    .filter(|errno| *errno > 0)
                    .unwrap_or(libc::EIO)
            };
            self.resources.close_returned(status);
        }
    }
}

#[cfg(test)]
pub(super) type FileObserver = MarkdownBidiResourceObserver;

/// All tables share one page set; stages/table count do not multiply the budget. The file stores only classes, levels, and coordinates.
pub(super) struct Store {
    io: Io,
    pages: Vec<Page>,
    bases: [u64; 5],
    capacity: u32,
    clock: u64,
    pub(super) page_loads: u64,
    poisoned: bool,
    cancellation: MarkdownBidiCancellation,
}

impl Store {
    #[cfg(test)]
    pub(super) fn new(capacity: u32, budget: BidiMemoryBudget) -> Result<Self, BidiError> {
        Self::new_cancellable(capacity, budget, MarkdownBidiCancellation::new())
    }

    #[cfg(test)]
    pub(super) fn new_cancellable(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        Self::new_cancellable_observed(capacity, budget, cancellation, None)
    }

    #[cfg(test)]
    pub(super) fn new_cancellable_observed(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: Option<&Arc<ResourceState>>,
    ) -> Result<Self, BidiError> {
        cancellation.check()?; // Reject an already cancelled task before creating the anonymous FD.
        let file = tempfile::tempfile()?;
        let io = match resources {
            Some(resources) => Io::with_resources(file, Arc::clone(resources)),
            None => Io::new(file), // This test constructor allocates observations only for actual Paged storage; Uniform adds no such owner.
        };
        Self::with_io_cancellable(capacity, budget, io, cancellation)
    }

    #[cfg(test)]
    fn with_io(capacity: u32, budget: BidiMemoryBudget, io: Io) -> Result<Self, BidiError> {
        Self::with_io_cancellable(capacity, budget, io, MarkdownBidiCancellation::new())
    }

    #[cfg(test)]
    fn with_io_cancellable(
        capacity: u32,
        budget: BidiMemoryBudget,
        io: Io,
        cancellation: MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        let mut store = Self::with_io_empty(capacity, budget, io, cancellation)?;
        for _ in 0..budget.pages {
            store.allocate_prepared_page()?;
        }
        Ok(store)
    }

    /// Only Prepare owns a candidate with an incomplete fixed-page allocation; no external Store or Ready is published before all pages initialize.
    pub(super) fn new_prepared_empty(
        capacity: u32,
        budget: BidiMemoryBudget,
        cancellation: MarkdownBidiCancellation,
        resources: &Arc<ResourceState>,
    ) -> Result<Self, BidiError> {
        cancellation.check()?;
        let file = tempfile::tempfile()?;
        Self::with_io_empty(
            capacity,
            budget,
            Io::with_resources(file, Arc::clone(resources)),
            cancellation,
        )
    }

    pub(super) fn allocate_prepared_page(&mut self) -> Result<(), BidiError> {
        self.cancellation.check()?;
        self.pages.push(Page {
            number: u64::MAX,
            bytes: PageBytes::new(&self.io.resources),
            dirty: false,
            used: 0,
        });
        #[cfg(test)]
        if self.pages.len() == 1 {
            self.cancellation.first_page_allocated(self.io.observer());
        }
        self.cancellation.check() // Retain the post-allocation cancellation point; failure drops the actual page owner without publishing the candidate.
    }

    pub(super) fn prepared_page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn prepared_page_dirty(&self, index: usize) -> bool {
        self.pages.get(index).is_some_and(|page| page.dirty)
    }

    pub(super) fn flush_prepared_page(&mut self, index: usize) -> Result<(), BidiError> {
        self.healthy()?;
        let page = self.pages.get_mut(index).ok_or(BidiError::InvalidBudget)?;
        if page.dirty
            && let Err(error) = self
                .io
                .write_page(page.bytes.as_slice(), page.number * PAGE_BYTES as u64)
        {
            self.poisoned = true;
            return Err(error.into());
        }
        page.dirty = false;
        self.healthy()
    }

    fn with_io_empty(
        capacity: u32,
        budget: BidiMemoryBudget,
        mut io: Io,
        cancellation: MarkdownBidiCancellation,
    ) -> Result<Self, BidiError> {
        cancellation.check()?; // If cancellation arrived while creating the FD, the local Io actually releases it before returning.
        let mut bases = [0; 5];
        let extent = u64::from(capacity)
            .checked_add(2)
            .and_then(|value| value.checked_mul(RECORD_BYTES))
            .ok_or(BidiError::CoordinateOverflow)?;
        // Separate tables on page boundaries; set_len establishes sparse logical length, without allocating text or whole-table RAM.
        let stride = extent
            .checked_add(PAGE_BYTES as u64 - 1)
            .map(|value| value / PAGE_BYTES as u64 * PAGE_BYTES as u64)
            .ok_or(BidiError::CoordinateOverflow)?;
        for (index, base) in bases.iter_mut().enumerate() {
            *base = stride
                .checked_mul(index as u64)
                .ok_or(BidiError::CoordinateOverflow)?;
        }
        // No queryable Store exists yet; a failing ? drops the I/O owner and returns its anonymous FD without publishing a partial object.
        io.set_len(
            stride
                .checked_mul(bases.len() as u64)
                .ok_or(BidiError::CoordinateOverflow)?,
        )?;
        cancellation.check()?; // Propagate set_len failure first; only successful actual I/O is followed by a new cancellation check.
        let pages = Vec::with_capacity(usize::from(budget.pages));
        // The already validated fixed budget sets page count; later misses can only reuse these actual buffers.

        Ok(Self {
            io,
            pages,
            bases,
            capacity,
            clock: 0,
            page_loads: 0,
            poisoned: false,
            cancellation,
        })
    }

    pub(super) fn healthy(&self) -> Result<(), BidiError> {
        if self.poisoned {
            Err(BidiError::PoisonedMetadata)
        } else {
            self.cancellation.check() // Existing metadata poison precedes later cancellation; the original I/O cause was returned at failure, and cached hits stay revoked.
        }
    }

    fn offset(&self, table: Table, id: u32) -> Result<u64, BidiError> {
        if id > self.capacity {
            return Err(BidiError::CorruptMetadata); // An invalid ID cannot cross a table boundary to access another valid record.
        }
        self.bases[table as usize]
            .checked_add(u64::from(id) * RECORD_BYTES)
            .ok_or(BidiError::CoordinateOverflow)
    }

    fn page(&mut self, number: u64) -> Result<usize, BidiError> {
        self.healthy()?; // Poison rejects cached hits too; resident fragments cannot bypass whole-store revocation.
        self.clock = self
            .clock
            .checked_add(1)
            .ok_or(BidiError::CoordinateOverflow)?;
        if let Some(index) = self.pages.iter().position(|page| page.number == number) {
            self.pages[index].used = self.clock;
            self.healthy()?;
            return Ok(index); // A hit neither reads the file nor creates another page copy.
        }
        let index = self
            .pages
            .iter()
            .enumerate()
            .min_by_key(|(_, page)| page.used)
            .map(|(index, _)| index)
            .ok_or(BidiError::InvalidBudget)?;
        let page = &mut self.pages[index];
        // Flush the actual dirty victim before reuse; failure poisons all tables rather than exposing a mixture of old and new metadata.
        if page.dirty
            && let Err(error) = self
                .io
                .write_page(page.bytes.as_slice(), page.number * PAGE_BYTES as u64)
        {
            self.poisoned = true;
            return Err(error.into());
        }
        self.cancellation.check()?;
        // Revoke the old identity before overwriting its buffer; a short read cannot leave the partly changed old page as a valid hit.
        page.number = u64::MAX;
        page.dirty = false;
        // The file has an actual logical extent; even unwritten sparse pages require a complete read, not a short read reinterpreted as zero metadata.
        if let Err(error) = self
            .io
            .read_page(page.bytes.as_mut_slice(), number * PAGE_BYTES as u64)
        {
            self.poisoned = true;
            return Err(error.into());
        }
        self.io.resources.page_loaded();
        self.cancellation.check()?; // Actual I/O completed, but cancellation still prevents publishing the new page identity.
        page.number = number;
        page.used = self.clock;
        page.dirty = false;
        self.page_loads += 1;
        Ok(index)
    }

    pub(super) fn read(&mut self, table: Table, id: u32) -> Result<[u8; 32], BidiError> {
        self.healthy()?;
        let offset = self.offset(table, id)?;
        let slot = self.page(offset / PAGE_BYTES as u64)?;
        let from = (offset % PAGE_BYTES as u64) as usize;
        let mut value = [0; 32];
        // Fixed records divide the page and do not cross it; copy only 32 metadata bytes and end the page borrow before returning.
        value.copy_from_slice(&self.pages[slot].bytes[from..from + 32]);
        self.healthy()?;
        Ok(value)
    }

    pub(super) fn write(
        &mut self,
        table: Table,
        id: u32,
        value: [u8; 32],
    ) -> Result<(), BidiError> {
        self.healthy()?;
        let offset = self.offset(table, id)?;
        let slot = self.page(offset / PAGE_BYTES as u64)?;
        let from = (offset % PAGE_BYTES as u64) as usize;
        self.pages[slot].bytes[from..from + 32].copy_from_slice(&value);
        self.pages[slot].dirty = true;
        self.healthy()
    }

    #[cfg(test)]
    pub(super) fn flush(&mut self) -> Result<(), BidiError> {
        self.healthy()?;
        // Use the same page writer; Prepare separately admits one resource operation before each actual dirty-page flush.
        for index in 0..self.pages.len() {
            self.flush_prepared_page(index)?;
        }
        self.healthy()
    }

    pub(super) fn cache_bytes(&self) -> usize {
        // Count actual allocated page payload only, excluding the small directory, Rust stack, and whole-process footprint/RSS.
        self.pages.len() * PAGE_BYTES
    }

    pub(super) fn resource_observer(&self) -> MarkdownBidiResourceObserver {
        self.io.resources.observe()
    }

    #[cfg(test)]
    pub(super) fn fail_next_read_after_prefix(&mut self, bytes: usize) {
        self.io.fault = Some(IoFault::PartialRead(bytes));
    }

    #[cfg(all(test, feature = "paged-layout-breaks"))]
    pub(super) fn fail_next_write_after_prefix(&mut self, bytes: usize) {
        self.io.fault = Some(IoFault::PartialWrite(bytes));
    }

    #[cfg(all(test, feature = "paged-layout-breaks"))]
    pub(super) fn io_observer_for_test(&self) -> FileObserver {
        self.io.observer()
    }

    #[cfg(test)]
    pub(super) fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.io.file().as_raw_fd()
    }
}

pub(super) fn read_u32(bytes: &[u8; 32], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("固定32B元数据字段"),
    )
}

pub(super) fn write_u32(bytes: &mut [u8; 32], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Initial FSI and suspended IRS stacks can exceed the explicit-level bound; page their entries too instead of truncating at 125.
pub(super) struct DiskStack {
    depth: u32,
}

impl DiskStack {
    pub(super) fn new() -> Self {
        Self { depth: 0 }
    }

    pub(super) fn len(&self) -> u32 {
        self.depth
    }

    pub(super) fn push(
        &mut self,
        store: &mut Store,
        first: u32,
        last: u32,
    ) -> Result<(), BidiError> {
        let mut bytes = [0; 32];
        // Commit the actual stack record before changing depth; a failed write must not expose an uninitialized top.
        write_u32(&mut bytes, 0, first);
        write_u32(&mut bytes, 4, last);
        store.write(Table::Stack, self.depth, bytes)?;
        self.depth = self
            .depth
            .checked_add(1)
            .ok_or(BidiError::CoordinateOverflow)?;
        Ok(())
    }

    pub(super) fn top(&self, store: &mut Store) -> Result<Option<(u32, u32)>, BidiError> {
        let Some(index) = self.depth.checked_sub(1) else {
            return Ok(None); // Empty stack is valid at paragraph start or unmatched PDI; do not read a nonexistent record.
        };
        let bytes = store.read(Table::Stack, index)?;
        Ok(Some((read_u32(&bytes, 0), read_u32(&bytes, 4))))
    }

    pub(super) fn pop(&mut self, store: &mut Store) -> Result<Option<(u32, u32)>, BidiError> {
        let result = self.top(store)?;
        if result.is_some() {
            self.depth -= 1;
        }
        Ok(result)
    }

    pub(super) fn clear(&mut self) {
        self.depth = 0; // Old slots contain only metadata for later overwrite, without retained heap capacity or Source owners.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_store() -> Store {
        Store::new(4096, BidiMemoryBudget::new(4 * PAGE_BYTES).unwrap()).unwrap()
    }

    #[test]
    fn production_receipt_waits_for_the_actual_fd_and_each_fixed_page_to_be_released() {
        // Keep the original Store live after cancellation, then observe actual FD/Box destruction rather than treating revocation as release.
        let mut store = small_store();
        let observer = store.resource_observer();
        let live = observer.snapshot();
        assert!(live.file_created && live.file_open && !live.close_succeeded);
        assert_eq!((live.cache_bytes, live.peak_cache_bytes), (65536, 65536));
        store.write(Table::Text, 0, [7; 32]).unwrap();
        store.cancellation.cancel();
        assert!(!observer.is_released()); // Cancellation revokes authority; the still-live file/pages have not actually returned.
        drop(store);
        let actual = observer.snapshot();
        assert!(actual.file_created && !actual.file_open && actual.close_succeeded);
        assert_eq!(actual.close_errno, None);
        assert_eq!((actual.cache_bytes, actual.peak_cache_bytes), (0, 65536));
        assert!(observer.is_released());
    }

    fn dirty_four_pages(store: &mut Store) {
        // Records are one actual page apart; four distinguishable dirty pages force the next miss to evict an original page.
        for page in 0..4 {
            store
                .write(Table::Text, page * 512, [0xc0 + page as u8; 32])
                .unwrap();
        }
    }

    #[test]
    fn cancellation_rejects_every_cached_operation_without_hiding_old_io_poison() {
        let mut store = small_store();
        store.write(Table::Text, 0, [7; 32]).unwrap();
        let observer = store.io.observer();
        store.cancellation.cancel();
        assert!(matches!(store.healthy(), Err(BidiError::Cancelled)));
        assert!(matches!(
            store.read(Table::Text, 0),
            Err(BidiError::Cancelled)
        ));
        assert!(matches!(
            store.write(Table::Text, 0, [9; 32]),
            Err(BidiError::Cancelled)
        ));
        assert!(matches!(store.flush(), Err(BidiError::Cancelled)));
        assert!(!observer.closed()); // Revocation is not destruction; the live original Store still owns its actual FD.
        drop(store);
        assert!(observer.closed());

        let mut store = small_store();
        store.write(Table::Text, 0, [7; 32]).unwrap();
        store.io.fault = Some(IoFault::PartialWrite(1));
        assert!(matches!(store.flush(), Err(BidiError::Storage(_))));
        store.cancellation.cancel();
        assert!(matches!(store.healthy(), Err(BidiError::PoisonedMetadata)));
    }

    #[test]
    fn partial_read_revokes_old_page_and_poison_rejects_even_untouched_cache_hits() {
        // Retain the existing partial-I/O seam and private buffer checks, alongside actual failure and permanent public Store-access rejection.
        let mut store = small_store();
        dirty_four_pages(&mut store);
        store.flush().unwrap();
        store.io.fault = Some(IoFault::PartialRead(8));
        assert!(matches!(
            store.read(Table::Text, 2048),
            Err(BidiError::Storage(_))
        ));
        let invalid = store
            .pages
            .iter()
            .find(|page| page.number == u64::MAX)
            .unwrap();
        assert_eq!(&invalid.bytes[..8], &[0; 8]);
        assert_eq!(&invalid.bytes[8..32], &[0xc0; 24]); // Actual prefix bytes changed before the injected error; this is not only a returned synthetic Err.
        assert!(matches!(
            store.read(Table::Text, 0),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(
            store.read(Table::Text, 512),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(
            store.write(Table::Text, 512, [0; 32]),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(store.flush(), Err(BidiError::PoisonedMetadata)));
    }

    #[test]
    fn partial_eviction_write_poison_rejects_cached_data_and_preserves_real_io_evidence() {
        let mut store = small_store();
        dirty_four_pages(&mut store);
        store.io.fault = Some(IoFault::PartialWrite(8));
        assert!(matches!(
            store.read(Table::Text, 2048),
            Err(BidiError::Storage(_))
        ));
        let mut actual = [0; 32];
        store.io.file().read_exact_at(&mut actual, 0).unwrap();
        assert_eq!(&actual[..8], &[0xc0; 8]);
        assert_eq!(&actual[8..], &[0; 24]); // Inspect only actual partial persistence of private metadata, without reading document text.
        assert!(matches!(
            store.read(Table::Text, 512),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(store.flush(), Err(BidiError::PoisonedMetadata)));
    }

    #[test]
    fn failed_final_flush_permanently_revokes_every_following_access() {
        // Final-flush failure must revoke later reads/writes even while the old cache still holds bytes.
        let mut store = small_store();
        store.write(Table::Text, 0, [0x77; 32]).unwrap();
        store.io.fault = Some(IoFault::PartialWrite(4));
        assert!(matches!(store.flush(), Err(BidiError::Storage(_))));
        assert!(matches!(
            store.read(Table::Text, 0),
            Err(BidiError::PoisonedMetadata)
        ));
        assert!(matches!(
            store.write(Table::Text, 0, [0; 32]),
            Err(BidiError::PoisonedMetadata)
        ));
    }

    #[test]
    fn successful_eviction_never_grows_the_real_page_payload_or_page_directory() {
        // Preserve this existing private directory-stability regression; actual metadata eviction runs through the production writer and fixed buffers.
        let mut store = small_store();
        let directory = store.pages.as_ptr();
        let capacity = store.pages.capacity();
        for index in 0..=4096 {
            store.write(Table::Text, index, [index as u8; 32]).unwrap();
        }
        store.flush().unwrap();
        assert!(store.page_loads > 4);
        assert_eq!(store.pages.as_ptr(), directory);
        assert_eq!(store.pages.capacity(), capacity);
        assert_eq!(store.pages.len(), 4);
        assert_eq!(store.cache_bytes(), 65536);
    }

    #[test]
    fn failed_length_initialization_returns_no_store_and_closes_its_actual_descriptor() {
        // Isolate descriptor-number reuse from parallel tests by waiting for the exact child test process; only its anonymous File is examined.
        const CHILD: &str = "OMNI_BIDI_INIT_IO_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "markdown_bidi::store::tests::failed_length_initialization_returns_no_store_and_closes_its_actual_descriptor", "--nocapture"])
                .env(CHILD, "1")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().unwrap();
        let fd = file.as_raw_fd();
        let mut io = Io::new(file);
        let resources = io.resources.observe();
        io.fault = Some(IoFault::SetLength);
        assert!(matches!(
            Store::with_io(8, BidiMemoryBudget::default(), io),
            Err(BidiError::Storage(_))
        ));
        // SAFETY: This child runs only this test and opens no replacement FD after failed construction; F_GETFD observes actual close without using a dropped Rust object.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        let actual = resources.snapshot();
        assert!(actual.file_created && actual.close_succeeded && resources.is_released());
        assert_eq!((actual.cache_bytes, actual.peak_cache_bytes), (0, 0));
    }
}
