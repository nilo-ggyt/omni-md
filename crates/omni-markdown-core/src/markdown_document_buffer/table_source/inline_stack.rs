//! Share one anonymous file for emphasis, real brackets and label checkpoints, retaining only its 2-KiB tail page.

use crate::markdown_document_buffer::table_window_request::{
    MarkdownTableRequestError, SemanticWork,
};
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota;
use std::ops::Range;
const PLAIN_CONTROL: usize = 128;
use pulldown_cmark::global_frontier::{
    GlobalWork, InlineLinkKind, InlineLinkRecord, InlineLinkRecordId, InlineLinkRecordStatus,
    InlineLinkStorageOperation, InlineLinkStorageReply, InlineLinkStorageRequest,
    InlineLinkStorageStep, InlineReferenceKind,
};
use pulldown_cmark::inline_emphasis::EmphasisBounds;
use std::{fs::File, io, os::fd::IntoRawFd, os::unix::fs::FileExt};

const PAGE: usize = 2048;
const SLOT: usize = 64;
const ENTRY: u8 = 1;
const LINK: u8 = 2;
const SCOPE: u8 = 3;
const CONTINUATION: u8 = 4;

#[derive(Clone, Copy)]
pub(in crate::markdown_document_buffer) struct Entry {
    // The original delimiter chain names only Entry records in this file.
    // Link/Scope tags have separate record meaning and cannot exchange IDs.
    pub previous: usize,
    pub depth: usize,
    pub start: usize,
    pub count: usize,
    pub run_length: usize,
    pub delimiter: u8,
    pub both: bool,
}

pub(in crate::markdown_document_buffer) struct Scope {
    // Source/label retain the original Begin's complete ownership range. The
    // remaining fields restore outer matching state from before entering that label.
    pub source: Range<usize>,
    pub label: Range<usize>,
    pub head: usize,
    pub depth: usize,
    pub bounds: EmphasisBounds,
    pub previous: Option<usize>,
}

pub(in crate::markdown_document_buffer) enum RecordStep<T> {
    // Progress records an actual page allocation or partial flush. Only a fully
    // readable record can return its value to the original cursor.
    Ready { value: T, work: SemanticWork },
    Progress { work: SemanticWork },
}

/// Count tracks actual 64-byte slots; flushed tracks successful writes from the current tail, not layout progress.
/// Multi-slot records never cross pages; real padding fills short tails without another page cache/directory.
pub(in crate::markdown_document_buffer) struct StackFile {
    file: Option<File>,
    page: Option<Box<[u8; PAGE]>>,
    count: usize,
    flushed: usize,
}
impl StackFile {
    pub(in crate::markdown_document_buffer) fn new() -> Self {
        // Neither the anonymous file nor its sole RAM page exists until actual
        // record work needs it. The transaction owns their release on every exit.
        Self {
            file: None,
            page: None,
            count: 0,
            flushed: 0,
        }
    }

    /// Admit the first real tail allocation before creating it, returning allocation and record work separately.
    /// Flush at most 1 KiB per step so a legal 2-KiB storage quantum can make actual progress.
    fn append_bytes(
        &mut self,
        bytes: &[u8],
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<RecordStep<usize>, MarkdownTableRequestError> {
        if budget < PLAIN_CONTROL {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        if self.page.is_none() {
            // This transaction retains one tail page only. Error/cancellation
            // returns it with the owner; successful publication uses explicit close.
            quota
                .admit_metadata(PAGE)
                .map_err(MarkdownTableRequestError::Window)?;
            self.page = Some(Box::new([0; PAGE]));
            work.metadata_bytes = PAGE;
            return Ok(RecordStep::Progress { work });
        }
        let mut used = self.count % (PAGE / SLOT) * SLOT;
        if used != 0 && used + bytes.len() > PAGE {
            // Padding is part of the real file format, not an issued record ID.
            // A later reader cannot reinterpret a partial Link as a complete Entry.
            let padding = PAGE - used;
            if work.structural_transitions + padding > budget {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            self.page.as_mut().unwrap()[used..].fill(0);
            self.count += padding / SLOT;
            work.structural_transitions += padding;
            used = 0;
        }
        if used == 0 && self.count != 0 && self.flushed != PAGE {
            // A full RAM page is not a completed pwrite. Advance flushed only
            // after the actual write succeeds, and never overwrite the tail before
            // that completion. Failure follows the parent's sticky path, without blind replay.
            if self.file.is_none() {
                self.file = Some(tempfile::tempfile().map_err(storage_error)?);
            }
            let count = (PAGE - self.flushed)
                .min(1024)
                .min(budget.saturating_sub(work.structural_transitions));
            if count == 0 {
                return Err(MarkdownTableRequestError::WorkLimit);
            }
            let offset = self.count * SLOT - PAGE + self.flushed;
            self.file
                .as_ref()
                .unwrap()
                .write_all_at(
                    &self.page.as_ref().unwrap()[self.flushed..self.flushed + count],
                    offset as u64,
                )
                .map_err(storage_error)?;
            self.flushed += count;
            work.structural_transitions += count;
            return Ok(RecordStep::Progress { work });
        }
        if work.structural_transitions + bytes.len() > budget {
            // Reject before modifying caller-visible record state. Allocation or
            // flush progress from earlier steps cannot authorize this oversized write.
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        self.page.as_mut().unwrap()[used..used + bytes.len()].copy_from_slice(bytes);
        // Allocation and flush are complete. Publish the readable slot watermark
        // only now, so an earlier Progress cannot expose half a record.
        let id = self.count + 1;
        self.count += bytes.len() / SLOT;
        self.flushed = 0;
        work.structural_transitions += bytes.len();
        Ok(RecordStep::Ready { value: id, work })
    }

    /// Read one fixed record from the original tail/FD; tags reject partial or differently typed records.
    fn read_bytes<const N: usize>(
        &self,
        id: usize,
        tag: u8,
    ) -> Result<[u8; N], MarkdownTableRequestError> {
        if id == 0
            || (id - 1)
                .checked_add(N / SLOT)
                .is_none_or(|end| end > self.count)
        {
            // A record must fit the actual published slot extent. Zero is the
            // format's absent-ID sentinel, never an address into the current page.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let offset = (id - 1) * SLOT;
        if offset % PAGE + N > PAGE {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        let mut bytes = [0; N];
        if offset / PAGE == (self.count - 1) * SLOT / PAGE {
            bytes.copy_from_slice(&self.page.as_ref().unwrap()[offset % PAGE..offset % PAGE + N]);
        } else {
            // Read exactly one old record from the original FD. Preserve real
            // short-read/errno failure rather than filling missing bytes with zeroes.
            self.file
                .as_ref()
                .ok_or(MarkdownTableRequestError::InvalidRequest)?
                .read_exact_at(&mut bytes, offset as u64)
                .map_err(storage_error)?;
        }
        if bytes[SLOT - 1] != tag
            || bytes
                .as_chunks::<SLOT>()
                .0
                .iter()
                .skip(1)
                .any(|part| part[SLOT - 1] != CONTINUATION)
        {
            // Every continuation slot must belong to this same typed record;
            // valid bounds alone do not authorize interpreting another record kind.
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok(bytes)
    }

    pub(in crate::markdown_document_buffer) fn append(
        &mut self,
        entry: Entry,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<RecordStep<usize>, MarkdownTableRequestError> {
        let mut bytes = [0; SLOT];
        // Emphasis retains its original 64-byte slot unit. Link/Scope records in
        // the same file do not change how the original predecessor chain is encoded.
        for (at, value) in [
            entry.previous,
            entry.depth,
            entry.start,
            entry.count,
            entry.run_length,
        ]
        .into_iter()
        .enumerate()
        {
            put(&mut bytes, at, value);
        }
        bytes[40] = entry.delimiter;
        bytes[41] = u8::from(entry.both);
        bytes[SLOT - 1] = ENTRY;
        self.append_bytes(&bytes, quota, budget)
    }
    pub(in crate::markdown_document_buffer) fn read(
        &self,
        id: usize,
    ) -> Result<Entry, MarkdownTableRequestError> {
        // Validate the original record type/extent before reconstructing the
        // delimiter state consumed by the real emphasis kernel.
        let bytes = self.read_bytes::<SLOT>(id, ENTRY)?;
        Ok(Entry {
            previous: get(&bytes, 0)?,
            depth: get(&bytes, 1)?,
            start: get(&bytes, 2)?,
            count: get(&bytes, 3)?,
            run_length: get(&bytes, 4)?,
            delimiter: bytes[40],
            both: bytes[41] != 0,
        })
    }

    /// Preserve all nine outer failure bounds; file ordinals are not delimiter depth and cannot replace them.
    pub(in crate::markdown_document_buffer) fn append_scope(
        &mut self,
        scope: &Scope,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<RecordStep<usize>, MarkdownTableRequestError> {
        let mut bytes = [0; 3 * SLOT];
        for (at, value) in [
            scope.source.start,
            scope.source.end,
            scope.label.start,
            scope.label.end,
            scope.head,
            scope.depth,
            scope.previous.unwrap_or(0),
        ]
        .into_iter()
        .chain(scope.bounds.checkpoint())
        .enumerate()
        {
            put(&mut bytes, at, value);
        }
        bytes[SLOT - 1] = SCOPE;
        bytes[2 * SLOT - 1] = CONTINUATION;
        bytes[3 * SLOT - 1] = CONTINUATION;
        self.append_bytes(&bytes, quota, budget)
    }
    pub(in crate::markdown_document_buffer) fn read_scope(
        &self,
        id: usize,
    ) -> Result<Scope, MarkdownTableRequestError> {
        let bytes = self.read_bytes::<{ 3 * SLOT }>(id, SCOPE)?;
        let mut bounds = [0; 9];
        // Restore the original kernel's nine failure bounds instead of restarting
        // at defaults that would change outer emphasis matching across a label.
        for (at, bound) in bounds.iter_mut().enumerate() {
            *bound = get(&bytes, at + 7)?;
        }
        Ok(Scope {
            source: get(&bytes, 0)?..get(&bytes, 1)?,
            label: get(&bytes, 2)?..get(&bytes, 3)?,
            head: get(&bytes, 4)?,
            depth: get(&bytes, 5)?,
            bounds: EmphasisBounds::restore(bounds),
            previous: match get(&bytes, 6)? {
                0 => None,
                id => Some(id),
            },
        })
    }

    /// Serve the vendor's real bracket stack through this sole storage port, without source parsing or another FD.
    /// Only actual record completion returns Ready to the same request; errors follow the parent's sticky path.
    pub(in crate::markdown_document_buffer) fn storage(
        &mut self,
        request: &InlineLinkStorageRequest,
        quota: &MarkdownTableWindowQuota,
        budget: usize,
    ) -> Result<(InlineLinkStorageStep, usize), MarkdownTableRequestError> {
        let (reply, work) = match &request.operation {
            InlineLinkStorageOperation::Append(record) => {
                match self.append_bytes(&encode_link(record), quota, budget)? {
                    RecordStep::Progress { work } => {
                        // Allocation/flush is actual progress, but no new link
                        // ID exists until append_bytes has published the whole record.
                        return Ok((
                            InlineLinkStorageStep::Progress {
                                work: GlobalWork {
                                    byte_reads: 0,
                                    transitions: work.structural_transitions,
                                },
                            },
                            work.metadata_bytes,
                        ));
                    }
                    RecordStep::Ready { value, work } => (
                        InlineLinkStorageReply::Appended {
                            id: InlineLinkRecordId::new(value)
                                .ok_or(MarkdownTableRequestError::InvalidRequest)?,
                        },
                        work,
                    ),
                }
            }
            InlineLinkStorageOperation::Read(id) => {
                // Admit before pread; performing I/O before rejecting the budget
                // would lose that actual work from the returned accounting.
                if budget < PLAIN_CONTROL + 2 * SLOT {
                    return Err(MarkdownTableRequestError::WorkLimit);
                }
                let record = decode_link(&self.read_bytes::<{ 2 * SLOT }>(id.get(), LINK)?)?;
                (
                    InlineLinkStorageReply::Read { record },
                    SemanticWork {
                        structural_transitions: PLAIN_CONTROL + 2 * SLOT,
                        ..Default::default()
                    },
                )
            }
            InlineLinkStorageOperation::Update { id, record } => {
                if budget < PLAIN_CONTROL + 4 * SLOT {
                    return Err(MarkdownTableRequestError::WorkLimit);
                }
                // Validate the actual original typed record before overwriting
                // its exact location, never an adjacent slot or arbitrary byte range.
                self.read_bytes::<{ 2 * SLOT }>(id.get(), LINK)?;
                let offset = (id.get() - 1) * SLOT;
                let bytes = encode_link(record);
                if offset / PAGE == (self.count - 1) * SLOT / PAGE {
                    // The same pending operation exclusively owns this tail page;
                    // no second cursor can update it during an incomplete flush.
                    self.page.as_mut().unwrap()[offset % PAGE..offset % PAGE + bytes.len()]
                        .copy_from_slice(&bytes);
                } else {
                    self.file
                        .as_ref()
                        .unwrap()
                        .write_all_at(&bytes, offset as u64)
                        .map_err(storage_error)?;
                }
                (
                    InlineLinkStorageReply::Updated,
                    SemanticWork {
                        structural_transitions: PLAIN_CONTROL + 4 * SLOT,
                        ..Default::default()
                    },
                )
            }
        };
        if work.structural_transitions > budget {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        Ok((
            InlineLinkStorageStep::Ready {
                reply,
                work: GlobalWork {
                    byte_reads: 0,
                    transitions: work.structural_transitions,
                },
            },
            work.metadata_bytes,
        ))
    }

    /// Return the actual FD before successful publication; never retry a close on a possibly reused descriptor.
    pub(in crate::markdown_document_buffer) fn close(
        &mut self,
    ) -> Result<(), MarkdownTableRequestError> {
        self.page = None;
        // Retire RAM and all issued IDs before ending FD ownership. Later use
        // starts from a new empty file rather than reviving old record permissions.
        self.count = 0;
        self.flushed = 0;
        if let Some(file) = self.file.take() {
            let descriptor = file.into_raw_fd();
            // SAFETY: The unique File transferred its sole close obligation;
            // no clone/reader remains, and this numeric descriptor is never closed again on failure.
            if unsafe { libc::close(descriptor) } != 0 {
                return Err(storage_error(io::Error::last_os_error()));
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::markdown_document_buffer) fn descriptor(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.file.as_ref().map(AsRawFd::as_raw_fd)
    }
    #[cfg(test)]
    pub(in crate::markdown_document_buffer) fn replace_file(&mut self, file: File) {
        self.file = Some(file);
    }
}

/// Keep the final byte of each slot for its tag; explicit little-endian fields skip tags across slot boundaries.
fn put(bytes: &mut [u8], field: usize, value: usize) {
    for (at, byte) in (value as u64).to_le_bytes().into_iter().enumerate() {
        let logical = field * 8 + at;
        bytes[logical + logical / (SLOT - 1)] = byte;
    }
}
fn get(bytes: &[u8], field: usize) -> Result<usize, MarkdownTableRequestError> {
    // Reconstruct only the fixed encoded integer and reject values this target
    // cannot represent; do not silently truncate a file-provided record coordinate.
    let mut value = [0; 8];
    for (at, byte) in value.iter_mut().enumerate() {
        let logical = field * 8 + at;
        *byte = bytes[logical + logical / (SLOT - 1)];
    }
    usize::try_from(u64::from_le_bytes(value))
        .map_err(|_| MarkdownTableRequestError::InvalidRequest)
}
fn encode_link(record: &InlineLinkRecord) -> [u8; 2 * SLOT] {
    // Two slots preserve owner, destination, title, reference and both actual
    // chains; fitting a single 64-byte slot must not discard real link semantics.
    let mut bytes = [0; 2 * SLOT];
    for (at, value) in [
        record.previous.map_or(0, InlineLinkRecordId::get),
        record.next.map_or(0, InlineLinkRecordId::get),
        record.serial,
        record.source_start,
        record.label_start,
        record.label_end,
        record.source_end,
        record.destination.start,
        record.destination.end,
        record.title.as_ref().map_or(0, |v| v.start),
        record.title.as_ref().map_or(0, |v| v.end),
        record.reference.as_ref().map_or(0, |(_, v)| v.start),
        record.reference.as_ref().map_or(0, |(_, v)| v.end),
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut bytes, at, value);
    }
    bytes[106] = u8::from(record.title.is_some());
    bytes[107] = match &record.reference {
        None => 0,
        Some((InlineReferenceKind::Full, _)) => 1,
        Some((InlineReferenceKind::Collapsed, _)) => 2,
        Some((InlineReferenceKind::Shortcut, _)) => 3,
        Some((InlineReferenceKind::Footnote, _)) => 4,
    };
    bytes[108] = match record.kind {
        InlineLinkKind::Link => 0,
        InlineLinkKind::Image => 1,
    };
    bytes[109] = match record.status {
        InlineLinkRecordStatus::Open => 0,
        InlineLinkRecordStatus::Literal => 1,
        InlineLinkRecordStatus::Direct => 2,
        InlineLinkRecordStatus::Reference => 3,
        InlineLinkRecordStatus::Footnote => 4,
    };
    bytes[SLOT - 1] = LINK;
    bytes[2 * SLOT - 1] = CONTINUATION;
    bytes
}
fn decode_link(bytes: &[u8; 2 * SLOT]) -> Result<InlineLinkRecord, MarkdownTableRequestError> {
    // Reject unknown enum encodings before constructing values. None uses only
    // the format's own sentinel; an invalid status cannot degrade into Literal.
    let reference = match bytes[107] {
        0 => None,
        1 => Some(InlineReferenceKind::Full),
        2 => Some(InlineReferenceKind::Collapsed),
        3 => Some(InlineReferenceKind::Shortcut),
        4 => Some(InlineReferenceKind::Footnote),
        _ => return Err(MarkdownTableRequestError::InvalidRequest),
    };
    Ok(InlineLinkRecord {
        previous: InlineLinkRecordId::new(get(bytes, 0)?),
        next: InlineLinkRecordId::new(get(bytes, 1)?),
        serial: get(bytes, 2)?,
        source_start: get(bytes, 3)?,
        label_start: get(bytes, 4)?,
        label_end: get(bytes, 5)?,
        source_end: get(bytes, 6)?,
        destination: get(bytes, 7)?..get(bytes, 8)?,
        title: match bytes[106] {
            0 => None,
            1 => Some(get(bytes, 9)?..get(bytes, 10)?),
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        },
        reference: match reference {
            Some(kind) => Some((kind, get(bytes, 11)?..get(bytes, 12)?)),
            None => None,
        },
        kind: match bytes[108] {
            0 => InlineLinkKind::Link,
            1 => InlineLinkKind::Image,
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        },
        status: match bytes[109] {
            0 => InlineLinkRecordStatus::Open,
            1 => InlineLinkRecordStatus::Literal,
            2 => InlineLinkRecordStatus::Direct,
            3 => InlineLinkRecordStatus::Reference,
            4 => InlineLinkRecordStatus::Footnote,
            _ => return Err(MarkdownTableRequestError::InvalidRequest),
        },
    })
}
fn storage_error(error: io::Error) -> MarkdownTableRequestError {
    // Preserve an actual OS errno when available; synthetic I/O kinds lacking
    // one use EIO within this existing typed storage-error boundary.
    MarkdownTableRequestError::InlineStorageUnavailable(error.raw_os_error().unwrap_or(libc::EIO))
}

impl std::fmt::Debug for StackFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StackFile")
            .field("slots", &self.count)
            .field("file_open", &self.file.is_some())
            .finish_non_exhaustive()
    }
}
