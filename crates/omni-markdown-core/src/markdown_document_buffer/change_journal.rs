//! The document owns its change journal. Up to two records stay inline; larger
//! commands use one 32 KiB page buffer and a fixed table of page owners.
//!
//! Disk records contain old coordinates and removed/inserted lengths, never text.
//! Candidate pages can be reclaimed on failure; committed history retains only
//! their small, non-cloneable receipt. The existing piece tree and session Source
//! continue to own the original root and actual document bytes.

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};

use sha2::{Digest, Sha256};

const PAGE_BYTES: usize = 32 * 1024;
const MAX_JOURNAL_BYTES: usize = 256 * 1024 * 1024;
const PAGE_COUNT: usize = MAX_JOURNAL_BYTES / PAGE_BYTES;
const HEADER_BYTES: usize = 32;
const RECORD_BYTES: usize = 24;
const RECORDS_PER_PAGE: usize = (PAGE_BYTES - HEADER_BYTES) / RECORD_BYTES;
const NO_PAGE: u32 = u32::MAX;
const MAGIC: &[u8; 8] = b"OMNICHG1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Preserve the finite I/O classification and errno without retaining an error
/// allocation, system message, path, or document content.
pub struct MarkdownChangeJournalIoFailure {
    pub kind: io::ErrorKind,
    pub raw_os_error: Option<i32>,
}

impl From<io::Error> for MarkdownChangeJournalIoFailure {
    fn from(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
/// Anonymous journal failures carry no content or paths. A caller must propagate
/// them through the command transaction rather than publish partial history.
pub enum MarkdownChangeJournalError {
    #[error("变化日志状态无效")]
    InvalidState,
    #[error("变化日志坐标无效")]
    InvalidChange,
    #[error("变化日志达到256MiB会话上限")]
    BudgetExceeded,
    #[error("无法创建私有匿名变化日志")]
    CreationUnavailable(MarkdownChangeJournalIoFailure),
    #[error("匿名变化日志的私有身份校验失败")]
    AnonymousIdentityRejected,
    #[error("无法完整写入变化日志")]
    WriteUnavailable(MarkdownChangeJournalIoFailure),
    #[error("无法完整读取变化日志")]
    ReadUnavailable(MarkdownChangeJournalIoFailure),
    #[error("变化日志内容校验失败")]
    Corrupt,
    #[error("变化日志失败后的截尾清理未确认")]
    TruncateUnavailable(MarkdownChangeJournalIoFailure),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Record {
    pub(super) start: usize,
    pub(super) removed: usize,
    pub(super) inserted: usize,
}

/// Move this receipt only inside its document. Keeping it non-Clone prevents two
/// history entries from independently releasing the same journal pages.
pub(super) struct Entry {
    old_length: usize,
    new_length: usize,
    payload: Payload,
}

impl Entry {
    /// A composition group's net replacement needs one inline record. Merging it
    /// neither borrows document bytes again nor opens a journal file.
    pub(super) fn single_replacement(
        old_length: usize,
        record: Record,
    ) -> Result<Self, MarkdownChangeJournalError> {
        let (old_end, new_end) = advance_coordinates(old_length, 0, 0, record)?;
        let new_length = new_end
            .checked_add(old_length - old_end)
            .ok_or(MarkdownChangeJournalError::InvalidChange)?;
        Ok(Self {
            old_length,
            new_length,
            payload: Payload::Inline {
                records: [record, Record::default()],
                count: usize::from(record.removed != 0 || record.inserted != 0),
            },
        })
    }
}

enum Payload {
    Inline {
        records: [Record; 2],
        count: usize,
    },
    Pages {
        id: u64,
        first: u32,
        pages: usize,
        records: usize,
        digest: [u8; 32],
    },
}

struct Pending {
    old_length: usize,
    old_cursor: usize,
    new_cursor: usize,
    inline: [Record; 2],
    records: usize,
    id: Option<u64>,
    first: u32,
    current: u32,
    pages: usize,
    page_records: usize,
    scratch: Option<Box<[u8; PAGE_BYTES]>>,
    hash: Sha256,
    file_length_before: u64,
    failure: Option<MarkdownChangeJournalError>,
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Fault {
    Create,
    Write,
    PartialWrite,
    Read,
    Truncate,
}

/// The document owns the only FD and fixed 64 KiB owner table. Both are acquired
/// only when a command reaches its third record; neither grows with match count.
pub(super) struct ChangeJournal {
    file: Option<File>,
    owners: Option<Box<[u64; PAGE_COUNT]>>,
    // Track the extent a write attempt may have touched. Inline checkpoints need
    // no stat or other I/O, and a failed partial write still has a cleanup bound.
    possible_extent: u64,
    next_id: u64,
    pending: Option<Pending>,
    page_limit: usize,
    #[cfg(test)]
    fault: std::cell::Cell<Option<Fault>>,
}

impl Default for ChangeJournal {
    fn default() -> Self {
        Self {
            file: None,
            owners: None,
            possible_extent: 0,
            next_id: 1,
            pending: None,
            page_limit: PAGE_COUNT,
            #[cfg(test)]
            fault: std::cell::Cell::new(None),
        }
    }
}

impl ChangeJournal {
    #[cfg(test)]
    pub(super) fn fail_next(&self, fault: Fault) {
        self.fault.set(Some(fault)); // This existing one-shot injection is absent from production builds.
    }

    /// Start the sole candidate without changing any live history page. The saved
    /// extent belongs to abort, so later writes cannot lose their truncation point.
    pub(super) fn begin(&mut self, old_length: usize) -> Result<(), MarkdownChangeJournalError> {
        if self.pending.is_some() {
            return Err(MarkdownChangeJournalError::InvalidState);
        }
        let file_length_before = self.possible_extent;
        self.pending = Some(Pending {
            old_length,
            old_cursor: 0,
            new_cursor: 0,
            inline: [Record::default(); 2],
            records: 0,
            id: None,
            first: NO_PAGE,
            current: NO_PAGE,
            pages: 0,
            page_records: 0,
            scratch: None,
            hash: Sha256::new(),
            file_length_before,
            failure: None,
        });
        Ok(())
    }

    pub(super) fn push(&mut self, record: Record) -> Result<(), MarkdownChangeJournalError> {
        if let Some(error) = self.pending.as_ref().and_then(|pending| pending.failure) {
            return Err(error); // Later records cannot repair a failed candidate into a different journal.
        }
        let result = self.push_inner(record);
        if let Err(error) = result
            && let Some(pending) = &mut self.pending
        {
            pending.failure = Some(error);
        }
        result
    }

    fn push_inner(&mut self, record: Record) -> Result<(), MarkdownChangeJournalError> {
        if let Some(pending) = self.pending.as_mut() {
            let (old_end, new_end) = advance_coordinates(
                pending.old_length,
                pending.old_cursor,
                pending.new_cursor,
                record,
            )?;
            let buffered = if pending.id.is_none() && pending.records < 2 {
                pending.inline[pending.records] = record;
                true
            } else if pending.id.is_some() && pending.page_records < RECORDS_PER_PAGE {
                Self::buffer_record(pending, record)?;
                true
            } else {
                false // Only promotion or a full page needs exclusive access to the file and page directory.
            };
            if buffered {
                // Common records update the current fields in place. They do not
                // move the hash, whole Pending value, or document bytes per match.
                pending.records = pending
                    .records
                    .checked_add(1)
                    .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
                pending.old_cursor = old_end;
                pending.new_cursor = new_end;
                return Ok(());
            }
        }
        // End the field borrow before touching the unique file. Restore Pending
        // even on failure so the owning transaction can abort all candidate pages.
        let mut pending = self
            .pending
            .take()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        let result = self.push_pending(&mut pending, record);
        self.pending = Some(pending);
        result
    }

    fn push_pending(
        &mut self,
        pending: &mut Pending,
        record: Record,
    ) -> Result<(), MarkdownChangeJournalError> {
        let (old_end, new_end) = advance_coordinates(
            pending.old_length,
            pending.old_cursor,
            pending.new_cursor,
            record,
        )?;
        if pending.records < 2 && pending.id.is_none() {
            pending.inline[pending.records] = record;
        } else {
            if pending.id.is_none() {
                // Promote immediately before the third record. Ordinary typing
                // and two-sided Surround edits need neither the FD nor owner table.
                self.ensure_file()?;
                let id = self.next_id;
                self.next_id = id
                    .checked_add(1)
                    .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
                pending.id = Some(id);
                pending.scratch = Some(Box::new([0; PAGE_BYTES]));
                let first = self.allocate_page(id)?;
                pending.first = first;
                pending.current = first;
                pending.pages = 1;
                for previous in pending.inline {
                    self.append_record(pending, previous)?;
                }
            }
            self.append_record(pending, record)?;
        }
        pending.records = pending
            .records
            .checked_add(1)
            .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
        pending.old_cursor = old_end;
        pending.new_cursor = new_end;
        Ok(())
    }

    fn ensure_file(&mut self) -> Result<(), MarkdownChangeJournalError> {
        if self.file.is_some() {
            return Ok(());
        }
        #[cfg(test)]
        self.check_fault(Fault::Create)
            .map_err(|error| MarkdownChangeJournalError::CreationUnavailable(error.into()))?;
        let file = tempfile::tempfile()
            .map_err(|error| MarkdownChangeJournalError::CreationUnavailable(error.into()))?;
        let metadata = file
            .metadata()
            .map_err(|error| MarkdownChangeJournalError::CreationUnavailable(error.into()))?;
        if metadata.nlink() != 0
            || metadata.permissions().mode() & 0o777 != 0o600
            || !metadata.is_file()
        {
            return Err(MarkdownChangeJournalError::AnonymousIdentityRejected);
        }
        // Allocate the directory only after verifying the actual anonymous 0600
        // file. A rejected local File drops without leaving a named artifact.
        self.owners = Some(Box::new([0; PAGE_COUNT]));
        self.file = Some(file);
        Ok(())
    }

    fn allocate_page(&mut self, id: u64) -> Result<u32, MarkdownChangeJournalError> {
        if self.page_limit > PAGE_COUNT {
            return Err(MarkdownChangeJournalError::BudgetExceeded); // A reduced test capacity cannot enlarge the real hard limit.
        }
        let owners = self
            .owners
            .as_mut()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        let page = owners[..self.page_limit]
            .iter()
            .position(|owner| *owner == 0)
            .ok_or(MarkdownChangeJournalError::BudgetExceeded)?;
        // Only an unowned slot can be assigned. A candidate cannot overwrite any
        // page still owned by the current undo or redo history.
        owners[page] = id;
        Ok(page as u32)
    }

    fn append_record(
        &mut self,
        pending: &mut Pending,
        record: Record,
    ) -> Result<(), MarkdownChangeJournalError> {
        if pending.page_records == RECORDS_PER_PAGE {
            // Reserve the successor before linking the full page. If flushing
            // fails, its owner ID still lets abort reclaim both reservations.
            let next =
                self.allocate_page(pending.id.ok_or(MarkdownChangeJournalError::InvalidState)?)?;
            self.flush_page(pending, next)?;
            pending.current = next;
            pending.pages += 1;
            pending.page_records = 0;
        }
        Self::buffer_record(pending, record)
    }

    fn buffer_record(
        pending: &mut Pending,
        record: Record,
    ) -> Result<(), MarkdownChangeJournalError> {
        // The caller has reserved one slot in this page. Fixed-width integers
        // encode coordinates only; no root, pointer, or text enters the file.
        let at = HEADER_BYTES + pending.page_records * RECORD_BYTES;
        let scratch = pending
            .scratch
            .as_mut()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        for (index, value) in [record.start, record.removed, record.inserted]
            .into_iter()
            .enumerate()
        {
            let value =
                u64::try_from(value).map_err(|_| MarkdownChangeJournalError::InvalidChange)?;
            scratch[at + index * 8..at + (index + 1) * 8].copy_from_slice(&value.to_le_bytes());
        }
        pending.page_records += 1;
        Ok(())
    }

    fn flush_page(
        &mut self,
        pending: &mut Pending,
        next: u32,
    ) -> Result<(), MarkdownChangeJournalError> {
        let scratch = pending
            .scratch
            .as_mut()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        scratch[..8].copy_from_slice(MAGIC);
        scratch[8..16].copy_from_slice(
            &pending
                .id
                .ok_or(MarkdownChangeJournalError::InvalidState)?
                .to_le_bytes(),
        );
        scratch[16..20].copy_from_slice(&next.to_le_bytes());
        scratch[20..24].copy_from_slice(&(pending.page_records as u32).to_le_bytes());
        scratch[24..28].copy_from_slice(&((pending.pages - 1) as u32).to_le_bytes());
        scratch[28..32].fill(0);
        #[cfg(test)]
        self.check_fault(Fault::Write)
            .map_err(|error| MarkdownChangeJournalError::WriteUnavailable(error.into()))?;
        // Page numbers come only from the fixed owner table. Validate the entire
        // pwrite extent before I/O so disk growth stays within the session limit.
        let offset = u64::from(pending.current) * PAGE_BYTES as u64;
        if offset + PAGE_BYTES as u64 > MAX_JOURNAL_BYTES as u64 {
            return Err(MarkdownChangeJournalError::BudgetExceeded);
        }
        // Record possible growth before writing: write_all_at may fail after
        // touching part of the page, and abort must not assume zero bytes changed.
        self.possible_extent = self.possible_extent.max(offset + PAGE_BYTES as u64);
        #[cfg(test)]
        if self.fault.get() == Some(Fault::PartialWrite) {
            // This existing test-only path writes a real partial page before
            // reporting failure, exercising cleanup of an actually touched file.
            self.fault.set(None);
            self.file
                .as_ref()
                .ok_or(MarkdownChangeJournalError::InvalidState)?
                .write_all_at(&scratch[..HEADER_BYTES + RECORD_BYTES], offset)
                .map_err(|error| MarkdownChangeJournalError::WriteUnavailable(error.into()))?;
            return Err(MarkdownChangeJournalError::WriteUnavailable(
                io::Error::other("synthetic partial write").into(),
            ));
        }
        self.file
            .as_ref()
            .ok_or(MarkdownChangeJournalError::InvalidState)?
            .write_all_at(scratch.as_slice(), offset)
            .map_err(|error| MarkdownChangeJournalError::WriteUnavailable(error.into()))?;
        // Hash only occupied record bytes after the complete page write succeeds.
        // The final receipt verifies logical history independently of page slack.
        pending
            .hash
            .update(&scratch[HEADER_BYTES..HEADER_BYTES + pending.page_records * RECORD_BYTES]);
        Ok(())
    }

    /// Seal exactly the command's resulting length. A failed final write keeps
    /// Pending available for the original checkpoint's abort; no receipt escapes.
    pub(super) fn finish(
        &mut self,
        new_length: usize,
    ) -> Result<Entry, MarkdownChangeJournalError> {
        let mut pending = self
            .pending
            .take()
            .ok_or(MarkdownChangeJournalError::InvalidState)?;
        let result = (|| {
            if let Some(error) = pending.failure {
                return Err(error); // Ignoring an earlier push failure cannot publish a partial journal.
            }
            let expected = pending
                .new_cursor
                .checked_add(pending.old_length - pending.old_cursor)
                .ok_or(MarkdownChangeJournalError::InvalidChange)?;
            if expected != new_length {
                return Err(MarkdownChangeJournalError::InvalidChange);
            }
            let payload = if let Some(id) = pending.id {
                self.flush_page(&mut pending, NO_PAGE)?;
                Payload::Pages {
                    id,
                    first: pending.first,
                    pages: pending.pages,
                    records: pending.records,
                    digest: pending.hash.clone().finalize().into(),
                }
            } else {
                Payload::Inline {
                    records: pending.inline,
                    count: pending.records,
                }
            };
            Ok(Entry {
                old_length: pending.old_length,
                new_length,
                payload,
            })
        })();
        if result.is_err() {
            self.pending = Some(pending); // The original checkpoint still owns cleanup after a final-page failure.
        }
        result
    }

    /// Revoke only this candidate's pages. Older history remains live; truncate
    /// only newly touched file extent when those older owners keep the FD open.
    pub(super) fn abort(&mut self) -> Result<(), MarkdownChangeJournalError> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if let Some(id) = pending.id {
            self.free_id(id);
        }
        if self.close_if_empty() {
            return Ok(()); // With no remaining page owner, drop the anonymous FD instead of truncating it.
        }
        if let Some(file) = &self.file
            && self.possible_extent > pending.file_length_before
        {
            #[cfg(test)]
            self.check_fault(Fault::Truncate)
                .map_err(|error| MarkdownChangeJournalError::TruncateUnavailable(error.into()))?;
            file.set_len(pending.file_length_before)
                .map_err(|error| MarkdownChangeJournalError::TruncateUnavailable(error.into()))?;
            self.possible_extent = pending.file_length_before;
        }
        Ok(())
    }

    pub(super) fn release(&mut self, entry: Entry) {
        if let Payload::Pages { id, .. } = entry.payload {
            self.free_id(id);
            self.close_if_empty();
        }
        // Committed-history release updates fixed metadata and drops the final
        // FD without a fallible write/truncate step. While owners remain, freed
        // pages are reused in the anonymous file, whose extent stays within 256 MiB.
    }

    fn free_id(&mut self, id: u64) {
        // Ownership, not page-chain contents, controls reclamation. Even a failed
        // flush or unreadable candidate cannot strand its reserved successor.
        if let Some(owners) = &mut self.owners {
            for owner in owners.iter_mut() {
                if *owner == id {
                    *owner = 0;
                }
            }
        }
    }

    fn close_if_empty(&mut self) -> bool {
        if self
            .owners
            .as_ref()
            .is_some_and(|owners| owners.iter().all(|owner| *owner == 0))
        {
            // No history or pending receipt can still refer to this file. Drop
            // its sole FD and owner allocation together before resetting extent.
            self.file = None;
            self.owners = None;
            self.possible_extent = 0;
            return true;
        }
        false
    }

    /// Replay ordered changes, optionally in their resulting-coordinate inverse.
    /// The observer must keep its output tentative until the whole visit succeeds:
    /// page-chain, digest, and final-length checks may fail after earlier records.
    pub(super) fn visit<E>(
        &self,
        entry: &Entry,
        inverse: bool,
        mut visit: impl FnMut(Record) -> Result<(), E>,
    ) -> Result<(), VisitError<E>> {
        let (mut old_cursor, mut new_cursor) = (0, 0);
        let mut emit = |record: Record| -> Result<(), VisitError<E>> {
            let (old_end, new_end) =
                advance_coordinates(entry.old_length, old_cursor, new_cursor, record)
                    .map_err(VisitError::Journal)?;
            let actual = if inverse {
                // Gaps survive unchanged; the insertion's start in new text is
                // derived from the same cursors rather than the old raw offset.
                Record {
                    start: new_end - record.inserted,
                    removed: record.inserted,
                    inserted: record.removed,
                }
            } else {
                record
            };
            visit(actual).map_err(VisitError::Observer)?;
            old_cursor = old_end;
            new_cursor = new_end;
            Ok(())
        };
        match &entry.payload {
            Payload::Inline { records, count } => {
                for record in &records[..*count] {
                    emit(*record)?;
                }
            }
            Payload::Pages {
                id,
                first,
                pages,
                records,
                digest,
            } => {
                // The non-Clone receipt and live owner table must agree before
                // reading any page. One fixed stack buffer serves the full visit.
                let owners = self
                    .owners
                    .as_ref()
                    .ok_or(VisitError::Journal(MarkdownChangeJournalError::Corrupt))?;
                let file = self
                    .file
                    .as_ref()
                    .ok_or(VisitError::Journal(MarkdownChangeJournalError::Corrupt))?;
                if *pages == 0 || *pages > self.page_limit || *records > *pages * RECORDS_PER_PAGE {
                    return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                }
                let mut buffer = [0u8; PAGE_BYTES];
                let mut page = *first;
                let mut read_records = 0;
                let mut hash = Sha256::new();
                for index in 0..*pages {
                    if page as usize >= self.page_limit || owners[page as usize] != *id {
                        return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                    }
                    #[cfg(test)]
                    self.check_fault(Fault::Read).map_err(|error| {
                        VisitError::Journal(MarkdownChangeJournalError::ReadUnavailable(
                            error.into(),
                        ))
                    })?;
                    file.read_exact_at(&mut buffer, u64::from(page) * PAGE_BYTES as u64)
                        .map_err(|error| {
                            VisitError::Journal(MarkdownChangeJournalError::ReadUnavailable(
                                error.into(),
                            ))
                        })?;
                    let next = u32_at(&buffer, 16);
                    let count = u32_at(&buffer, 20) as usize;
                    // Reject a wrong owner/order, partial middle page, or broken
                    // terminal link before interpreting that page's coordinates.
                    if &buffer[..8] != MAGIC
                        || u64_at(&buffer, 8) != *id
                        || u32_at(&buffer, 24) as usize != index
                        || u32_at(&buffer, 28) != 0
                        || count == 0
                        || count > RECORDS_PER_PAGE
                        || index + 1 < *pages && count != RECORDS_PER_PAGE
                        || (index + 1 == *pages) != (next == NO_PAGE)
                    {
                        return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                    }
                    let payload = &buffer[HEADER_BYTES..HEADER_BYTES + count * RECORD_BYTES];
                    hash.update(payload);
                    for bytes in payload.as_chunks::<RECORD_BYTES>().0 {
                        emit(Record {
                            start: usize_at(bytes, 0)?,
                            removed: usize_at(bytes, 8)?,
                            inserted: usize_at(bytes, 16)?,
                        })?;
                    }
                    read_records += count;
                    page = next;
                }
                let actual: [u8; 32] = hash.finalize().into();
                // The digest covers all occupied records in order, including a
                // short final page; matching headers alone cannot seal history.
                if read_records != *records || actual != *digest {
                    return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
                }
            }
        }
        // Unchanged trailing text participates in the command's final length.
        // A valid record prefix cannot stand in for the sealed full transformation.
        if new_cursor.checked_add(entry.old_length - old_cursor) != Some(entry.new_length) {
            return Err(VisitError::Journal(MarkdownChangeJournalError::Corrupt));
        }
        Ok(())
    }

    #[cfg(test)]
    fn check_fault(&self, operation: Fault) -> io::Result<()> {
        if self.fault.get() == Some(operation) {
            self.fault.set(None);
            return Err(io::Error::other("synthetic journal failure"));
        }
        Ok(())
    }
}

pub(super) enum VisitError<E> {
    Journal(MarkdownChangeJournalError),
    Observer(E),
}

/// Validate monotonic, non-overlapping old coordinates and derive the matching
/// new cursor with checked arithmetic before any record is buffered or emitted.
fn advance_coordinates(
    old_length: usize,
    old: usize,
    new: usize,
    record: Record,
) -> Result<(usize, usize), MarkdownChangeJournalError> {
    let old_end = record
        .start
        .checked_add(record.removed)
        .filter(|end| *end <= old_length)
        .ok_or(MarkdownChangeJournalError::InvalidChange)?;
    let gap = record
        .start
        .checked_sub(old)
        .ok_or(MarkdownChangeJournalError::InvalidChange)?;
    let new_end = new
        .checked_add(gap)
        .and_then(|start| start.checked_add(record.inserted))
        .ok_or(MarkdownChangeJournalError::InvalidChange)?;
    Ok((old_end, new_end))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("fixed journal field"))
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("fixed journal field"))
}
fn usize_at<E>(bytes: &[u8], at: usize) -> Result<usize, VisitError<E>> {
    usize::try_from(u64_at(bytes, at))
        .map_err(|_| VisitError::Journal(MarkdownChangeJournalError::Corrupt))
}

#[cfg(test)]
mod tests;
