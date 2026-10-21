//! Default Unicode18 / UAX14 revision57 opportunities; width selection, dictionary segmentation, locale tailoring, and Native layout have separate owners.
//!
//! Borrow bounded pieces of one frozen Source. Pending rules retain fixed property state, and actual opportunities enter an independently owned anonymous paged index.
//! NoBreak outcomes are not stored per scalar, and chunk end is not text end; enabling a probe feature alone does not prove product-path integration.

use crate::{
    MarkdownDocumentBufferError, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceObserver,
    markdown_layout_index::{LayoutIndexError, LayoutIndexObserver},
};
use thiserror::Error;

#[cfg(feature = "paged-line-compose")]
use crate::markdown_layout_source::folds::FoldPlan;

mod machine;
mod store;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod table_rows;
mod unicode_data;

pub const MARKDOWN_LINE_BREAK_SOURCE_BYTES: usize = 4096;
pub const MARKDOWN_LINE_BREAK_INDEX_BYTES: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum MarkdownLineBreakError {
    #[error("默认断行扫描配额无效")]
    InvalidBudget,
    #[error("默认断行查询范围无效")]
    InvalidRange,
    #[error("折叠内部没有可见默认断行查询权限")]
    FoldedRange,
    #[error("独立表格行内部须查询该格的显示断行来源")]
    TableRange,
    #[error("默认断行源权限不匹配")]
    SourceMismatch,
    #[error("默认断行坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结正文不是完整UTF-8")]
    InvalidUtf8,
    #[error("默认断行索引元数据不完整")]
    CorruptMetadata,
    #[error("默认断行任务已永久撤权")]
    Poisoned,
    #[error("无法借用默认断行的冻结正文")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名默认断行索引")]
    Storage(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, MarkdownLineBreakError>;

impl From<LayoutIndexError> for MarkdownLineBreakError {
    fn from(value: LayoutIndexError) -> Self {
        // Keep actual storage errors typed while mapping invalid record structure and arithmetic overflow to this index's public contract.
        match value {
            LayoutIndexError::InvalidRecord => Self::CorruptMetadata,
            LayoutIndexError::Overflow => Self::CoordinateOverflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}
impl From<machine::Error> for MarkdownLineBreakError {
    fn from(value: machine::Error) -> Self {
        // Decoder/order failure cannot become a valid partial index; the owning job revokes publication after receiving this cause.
        match value {
            machine::Error::OutOfOrder => Self::CorruptMetadata,
            machine::Error::InvalidUtf8 => Self::InvalidUtf8,
            machine::Error::OffsetOverflow => Self::CoordinateOverflow,
            machine::Error::Closed | machine::Error::Poisoned => Self::Poisoned,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakKind {
    Allowed,
    Mandatory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLineBreakBoundary {
    pub byte: usize,
    pub utf16: usize,
    pub kind: MarkdownLineBreakKind,
    /// Already read text includes SA requiring dictionary work; false does not prove an unread successor contains no SA.
    pub needs_dictionary: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakQuery {
    IsBoundary,
    Previous,
    Next,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakAnswer {
    Pending,
    Known(Option<MarkdownLineBreakBoundary>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownLineBreakStatus {
    Pending,
    Ready,
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
}
impl Default for MarkdownLineBreakBudget {
    fn default() -> Self {
        Self {
            bytes: 4096,
            scalars: 1024,
            records: 1024,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakStep {
    pub status: MarkdownLineBreakStatus,
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub needs_dictionary: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakStorageProfile {
    pub generation: u64,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    pub boundary_records: usize,
    pub index_page_loads: u64,
    pub index_peak_mapped_bytes: usize,
    pub machine_bytes: usize,
    pub needs_dictionary: bool,
    pub unicode_version: (u8, u8, u8),
    pub revision: u8,
}

#[derive(Clone)]
pub struct MarkdownLineBreakResourceObserver {
    index: LayoutIndexObserver,
    source: MarkdownLayoutTextSourceObserver,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownLineBreakResourceSnapshot {
    pub metadata_file_open: bool,
    pub metadata_map_bytes: usize,
    pub metadata_peak_map_bytes: usize,
    /// Current/historical actual heap bytes of the anonymous metadata's sole write buffer, measured separately from document mappings.
    pub metadata_buffer_bytes: usize,
    pub metadata_peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    pub source_payload_released: bool,
}
impl MarkdownLineBreakResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownLineBreakResourceSnapshot {
        let index = self.index.snapshot();
        MarkdownLineBreakResourceSnapshot {
            metadata_file_open: index.file_open,
            metadata_map_bytes: index.mapped_bytes,
            metadata_peak_map_bytes: index.peak_mapped_bytes,
            metadata_buffer_bytes: index.buffer_bytes,
            metadata_peak_buffer_bytes: index.peak_buffer_bytes,
            cleanup_failed: index.cleanup_failed,
            source_payload_released: self.source.payload_released(),
        }
    }
}

/// Own one frozen Source claim and a paged index of 32-byte opportunities, without a second body, glyph inventory, or global registry.
pub struct MarkdownLineBreakJob {
    index: store::LineBreakIndex,
    source: MarkdownLayoutTextSource,
    machine: machine::Machine,
    failed: bool,
    ready: bool,
    source_borrow_max: usize,
    // A hidden-range jump changes canonical coordinates only; those coordinates cannot stand in for actual source-byte consumption.
    source_bytes_read: usize,
    #[cfg(feature = "paged-line-compose")]
    folds: Option<FoldPlan>,
    #[cfg(feature = "paged-line-compose")]
    next_fold: usize,
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    tables: Option<table_rows::State>,
    #[cfg(test)]
    split: Option<usize>,
}
impl MarkdownLineBreakJob {
    /// Create the lazy metadata owner without opening its FD or scanning Source; callers can share the same immutable payload with the GCB job.
    ///
    /// # Errors
    /// Reject an unsupported fixed-record layout; anonymous-file creation errors belong to the later operation that actually needs the FD.
    pub fn new(source: MarkdownLayoutTextSource) -> Result<Self> {
        Ok(Self {
            index: store::LineBreakIndex::new()?,
            source,
            machine: machine::Machine::new(),
            failed: false,
            ready: false,
            source_borrow_max: 0,
            source_bytes_read: 0,
            #[cfg(feature = "paged-line-compose")]
            folds: None,
            #[cfg(feature = "paged-line-compose")]
            next_fold: 0,
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            tables: None,
            #[cfg(test)]
            split: None,
        })
    }

    /// Display composition shares same-Source endpoint proofs before the first scan quantum; ordinary construction still analyzes the complete text incrementally.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn install_fold_plan(&mut self, plan: FoldPlan) -> Result<()> {
        self.guard(plan.authority())?;
        if self.source_bytes_read != 0 || self.ready || self.folds.is_some() {
            self.fail();
            return Err(MarkdownLineBreakError::InvalidRange);
        }
        self.folds = Some(plan);
        Ok(())
    }

    /// Settle original pending tokens only at an actually read header line ending; ordinary UTF8/LB rules still consume the hidden range's tail.
    #[cfg(feature = "paged-line-compose")]
    fn advance_fold(&mut self, records: &mut usize, budget: usize) -> Result<()> {
        let Some(span) = self
            .folds
            .as_ref()
            .and_then(|plan| plan.spans().get(self.next_fold))
            .copied()
        else {
            return Ok(());
        };
        if self.machine.byte_offset() != span.header_end.byte as u64 {
            return Ok(());
        }
        // If both endpoints share one terminator there is no interior to skip; continue ordinary decoding while still filtering hidden opportunities.
        if span.tail_start.byte <= span.header_end.byte {
            self.next_fold += 1;
            return Ok(());
        }
        while *records < budget && !self.machine.fold_header_drained()? {
            if let Some(event) = self.machine.drain_fold_header_one()? {
                self.index.push(event, self.machine.needs_dictionary())?;
                *records += 1;
            }
        }
        if self.machine.fold_header_drained()? {
            self.machine.resume_at_fold_tail(span)?;
            self.next_fold += 1;
        }
        Ok(())
    }
    fn guard(&mut self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        if let Err(error) = self.source.health() {
            // A reclaim failure in this frozen Source permanently revokes both published-prefix and Ready authority; return that actual Source cause.
            self.fail();
            return Err(MarkdownLineBreakError::Source(error));
        }
        if self.failed {
            return Err(MarkdownLineBreakError::Poisoned);
        }
        if !self.source.permits(authority) {
            self.fail();
            return Err(MarkdownLineBreakError::SourceMismatch);
        }
        Ok(())
    }
    fn fail(&mut self) {
        // Poison the actual derived index, including cached records, rather than allowing a failed prefix to remain queryable.
        self.failed = true;
        self.index.poison();
    }

    /// Charge all three quotas; a prefix opportunity is not complete Ready, and failure permanently revokes the whole derived index.
    ///
    /// # Errors
    /// Return typed causes for foreign Source, zero quotas, invalid UTF8/coordinates, arithmetic overflow, or actual index I/O failure.
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownLineBreakBudget,
    ) -> Result<MarkdownLineBreakStep> {
        self.guard(authority)?;
        let result = self.step_inner(budget);
        // Real shared-Source health is checked after all work, so metadata failure cannot mask an original source failure discovered during the quantum.
        let result = self
            .source
            .health()
            .map_err(MarkdownLineBreakError::from)
            .and(result);
        if result.is_err() {
            self.fail();
        }
        result
    }

    fn step_inner(&mut self, budget: MarkdownLineBreakBudget) -> Result<MarkdownLineBreakStep> {
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
            return Err(MarkdownLineBreakError::InvalidBudget);
        }
        let mut step = MarkdownLineBreakStep {
            status: MarkdownLineBreakStatus::Pending,
            bytes: 0,
            scalars: 0,
            records: 0,
            needs_dictionary: self.machine.needs_dictionary(),
        };
        if self.ready {
            // Completion is cached only after actual EOF and index sealing; the outer guard still checks the original Source on every call.
            step.status = MarkdownLineBreakStatus::Ready;
            return Ok(step);
        }
        while usize::try_from(self.machine.byte_offset())
            .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?
            < self.source.byte_len()
            && step.bytes < budget.bytes
            && step.scalars < budget.scalars
            && step.records < budget.records
        {
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            if table_rows::advance(self, budget, &mut step)? {
                return Ok(step);
            }
            #[cfg(feature = "paged-line-compose")]
            self.advance_fold(&mut step.records, budget.records)?;
            if step.records == budget.records
                || self.machine.byte_offset() == self.source.byte_len() as u64
            {
                break;
            }
            let start = usize::try_from(self.machine.byte_offset())
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
            // One byte completes at most one scalar/opportunity. Borrow at most the minimum remaining quota and consume the actual piece callback.
            // Do not read 4 KiB first and discard uncharged text, or use a fabricated error to end successful visitation early.
            let size = (budget.bytes - step.bytes)
                .min(budget.scalars - step.scalars)
                .min(budget.records - step.records)
                .min(MARKDOWN_LINE_BREAK_SOURCE_BYTES)
                .min(self.source.byte_len() - start);
            #[cfg(feature = "paged-line-compose")]
            let size = if let Some(span) = self
                .folds
                .as_ref()
                .and_then(|plan| plan.spans().get(self.next_fold))
            {
                size.min(
                    span.header_end
                        .byte
                        .checked_sub(start)
                        .ok_or(MarkdownLineBreakError::CorruptMetadata)?,
                )
            } else {
                size
            };
            let end = start
                .checked_add(size)
                .ok_or(MarkdownLineBreakError::CoordinateOverflow)?;
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            let end = table_rows::read_end(self, end);
            #[cfg(test)]
            let end = self
                .split
                .filter(|split| start < *split && *split < end)
                .unwrap_or(end);
            self.source_borrow_max = self.source_borrow_max.max(end - start);
            let before_byte = self.machine.byte_offset();
            let before_scalars = self.machine.scalar_count();
            let mut failure = None;
            let machine = &mut self.machine;
            let index = &mut self.index;
            #[cfg(feature = "paged-line-compose")]
            let folds = &self.folds;
            let source_result = self.source.visit_byte_range(start..end, |bytes| {
                if failure.is_some() {
                    return Ok(());
                } // Stop business decoding/writes after the first local failure; a later callback does not regain publication authority.
                for byte in bytes {
                    if failure.is_some() {
                        break;
                    } // After actual I/O failure, later bytes cannot acquire permission to append metadata.
                    let mut event = None;
                    let mut duplicate = false;
                    let result =
                        machine.push(machine.byte_offset(), std::slice::from_ref(byte), |value| {
                            duplicate |= event.replace(value).is_some();
                        });
                    if let Err(error) = result {
                        failure = Some(error.into());
                        break;
                    }
                    if duplicate {
                        failure = Some(MarkdownLineBreakError::CorruptMetadata);
                        break;
                    }
                    if let Some(event) = event {
                        #[cfg(feature = "paged-line-compose")]
                        if folds
                            .as_ref()
                            .is_some_and(|plan| plan.hides(event.utf16_offset as usize, true))
                        {
                            continue;
                        }
                        // Each byte completes at most one scalar/opportunity. End the mutable machine borrow before appending its original SA fact.
                        // Default AL class resolution does not certify completed dictionary segmentation.
                        match index.push(event, machine.needs_dictionary()) {
                            Ok(()) => step.records += 1,
                            Err(error) => failure = Some(error),
                        }
                    }
                }
                Ok(())
            });
            // Charge the actual prefix decoded before failure too; skipped source coordinates contribute no consumed bytes or scalars.
            let consumed = usize::try_from(self.machine.byte_offset() - before_byte)
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
            self.source_bytes_read = self
                .source_bytes_read
                .checked_add(consumed)
                .ok_or(MarkdownLineBreakError::CoordinateOverflow)?;
            step.bytes += consumed;
            step.scalars += usize::try_from(self.machine.scalar_count() - before_scalars)
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
            if let Some(error) = failure {
                return Err(error);
            }
            source_result?;
        }
        if usize::try_from(self.machine.byte_offset())
            .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?
            == self.source.byte_len()
        {
            if usize::try_from(self.machine.utf16_offset())
                .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?
                != self.source.utf16_len()
            {
                return Err(MarkdownLineBreakError::InvalidUtf8); // Exact Source EOF must match original UTF16 totals; a short read cannot manufacture text end.
            }
            while step.records < budget.records && !self.machine.finished() {
                // Drain actual remaining rules and the terminal opportunity under the record quota, preserving pending state between steps.
                if let Some(event) = self.machine.finish_one()? {
                    self.index.push(event, self.machine.needs_dictionary())?;
                    step.records += 1;
                }
            }
            if self.machine.finished() {
                // The actual writer must seal successfully before the job publishes Ready.
                self.index.seal()?;
                self.ready = true;
            }
        }
        step.needs_dictionary = self.machine.needs_dictionary();
        step.status = if self.ready {
            MarkdownLineBreakStatus::Ready
        } else {
            MarkdownLineBreakStatus::Pending
        };
        Ok(step)
    }

    /// Query only the original Source's certified prefix; Previous/Next are strict, and an unknown successor remains Pending.
    ///
    /// # Errors
    /// Out-of-range, foreign-authority, or metadata I/O errors revoke the index; interior UTF8/low-surrogate coordinates cannot become boundaries.
    pub fn query(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownLineBreakUnit,
        offset: usize,
        query: MarkdownLineBreakQuery,
    ) -> Result<MarkdownLineBreakAnswer> {
        self.guard(authority)?;
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(tables) = &self.tables
            && tables
                .cursor
                .plan
                .contains(offset, matches!(unit, MarkdownLineBreakUnit::Utf16))?
        {
            return Err(MarkdownLineBreakError::TableRange);
        }
        #[cfg(feature = "paged-line-compose")]
        if self
            .folds
            .as_ref()
            .is_some_and(|plan| plan.hides(offset, matches!(unit, MarkdownLineBreakUnit::Utf16)))
        {
            // Hidden text is valid, but this display configuration grants no query authority there; refusal does not revoke a healthy visible prefix.
            return Err(MarkdownLineBreakError::FoldedRange);
        }
        let result = self.query_inner(unit, offset, query);
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        let result = result.and_then(|answer| {
            if let (MarkdownLineBreakAnswer::Known(Some(value)), Some(state)) =
                (&answer, &self.tables)
                && state.cursor.plan.contains(value.utf16, true)?
            {
                return Err(MarkdownLineBreakError::TableRange);
            }
            Ok(answer)
        });
        let result = self
            .source
            .health()
            .map_err(MarkdownLineBreakError::from)
            .and(result);
        if result.as_ref().is_err_and(|error| {
            !matches!(
                error,
                MarkdownLineBreakError::TableRange | MarkdownLineBreakError::FoldedRange
            )
        }) {
            self.fail();
        }
        result
    }
    fn query_inner(
        &mut self,
        unit: MarkdownLineBreakUnit,
        offset: usize,
        query: MarkdownLineBreakQuery,
    ) -> Result<MarkdownLineBreakAnswer> {
        let position = |value: MarkdownLineBreakBoundary| match unit {
            MarkdownLineBreakUnit::Byte => value.byte,
            MarkdownLineBreakUnit::Utf16 => value.utf16,
        };
        let maximum = match unit {
            MarkdownLineBreakUnit::Byte => self.source.byte_len(),
            MarkdownLineBreakUnit::Utf16 => self.source.utf16_len(),
        };
        if offset > maximum {
            return Err(MarkdownLineBreakError::InvalidRange);
        }
        let (byte, utf16) = self.machine.confirmed_offsets();
        let frontier = usize::try_from(match unit {
            MarkdownLineBreakUnit::Byte => byte,
            MarkdownLineBreakUnit::Utf16 => utf16,
        })
        .map_err(|_| MarkdownLineBreakError::CoordinateOverflow)?;
        let mut low = 0;
        let mut high = self.index.len();
        // Retain only the neighboring opportunities actually read by this query, then return them without rereading the same record.
        // These locals do not survive the query, replace Source health checks, or publish an uncertified successor.
        let mut previous = None;
        let mut following = None;
        if let Some(page) = self.index.append_page_records()?
            && !page.is_empty()
        {
            // Forward layout often queries near the original append page. Its first actual record bounds binary search to initialized records.
            // Older regions still use the original FD, without another page cache, early body reads, or invented records.
            let first = self.index.get(page.start)?;
            if position(first) < offset {
                low = page.start + 1;
                high = page.end;
                previous = Some(first);
            } else {
                high = page.start;
                following = Some(first); // The strict successor of an older position may be exactly this append page's first opportunity.
                if position(first) == offset {
                    low = page.start;
                    if matches!(query, MarkdownLineBreakQuery::Previous) && page.start != 0 {
                        // The page's first record is not its own strict predecessor; read the actual previous record from the original FD.
                        previous = Some(self.index.get(page.start - 1)?);
                    }
                }
            }
        }
        while low < high {
            let middle = low + (high - low) / 2;
            let boundary = self.index.get(middle)?;
            if position(boundary) < offset {
                low = middle + 1;
                previous = Some(boundary);
            } else {
                high = middle;
                following = Some(boundary);
            }
        }
        match query {
            MarkdownLineBreakQuery::IsBoundary => {
                if let Some(value) = following.filter(|value| position(*value) == offset) {
                    return Ok(MarkdownLineBreakAnswer::Known(Some(value)));
                }
                if self.ready || offset < frontier || (offset == 0 && self.source.byte_len() != 0) {
                    Ok(MarkdownLineBreakAnswer::Known(None))
                } else {
                    Ok(MarkdownLineBreakAnswer::Pending)
                }
            }
            MarkdownLineBreakQuery::Previous => {
                if !self.ready && offset > frontier {
                    return Ok(MarkdownLineBreakAnswer::Pending);
                }
                Ok(MarkdownLineBreakAnswer::Known(previous))
            }
            MarkdownLineBreakQuery::Next => {
                if let Some(boundary) = following {
                    if position(boundary) != offset {
                        return Ok(MarkdownLineBreakAnswer::Known(Some(boundary)));
                    }
                    // Next remains strictly greater; equality requires the actual next record instead of reusing the current opportunity.
                    low += 1;
                }
                if low < self.index.len() {
                    return Ok(MarkdownLineBreakAnswer::Known(Some(self.index.get(low)?)));
                }
                Ok(if self.ready {
                    MarkdownLineBreakAnswer::Known(None)
                } else {
                    MarkdownLineBreakAnswer::Pending
                })
            }
        }
    }

    pub fn cancel(&mut self) {
        // Revoke all derived opportunities immediately; explicit close or Drop still owns actual index/source resource release.
        self.fail();
    }
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.failed && self.ready
    }
    #[must_use]
    pub fn storage_profile(&self) -> MarkdownLineBreakStorageProfile {
        MarkdownLineBreakStorageProfile {
            generation: self.source.generation(),
            source_bytes: self.source.byte_len(),
            source_utf16: self.source.utf16_len(),
            source_bytes_read: self.source_bytes_read,
            source_borrow_max_bytes: self.source_borrow_max,
            boundary_records: self.index.len(),
            index_page_loads: self.index.page_loads(),
            index_peak_mapped_bytes: self.index.resource_observer().snapshot().peak_mapped_bytes,
            machine_bytes: std::mem::size_of::<machine::Machine>(),
            needs_dictionary: self.machine.needs_dictionary(),
            unicode_version: (18, 0, 0),
            revision: 57,
        }
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownLineBreakResourceObserver {
        MarkdownLineBreakResourceObserver {
            index: self.index.resource_observer(),
            source: self.source.resource_observer(),
        }
    }
    /// Return this index's actual FD and sole write buffer; other Source clones remain independently valid and their immutable text is not cleared.
    ///
    /// # Errors
    /// Actual write/close failures retain negative resource facts; prior poison cannot turn failed cleanup into a success receipt.
    pub fn close(mut self) -> Result<()> {
        self.index.close()
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-line-compose",
    feature = "paged-shaping-frontier"
))]
impl MarkdownLineBreakJob {
    /// A structural jump completes only after the original machine consumes the actual row tail; an unconsumed endpoint remains pending.
    pub(crate) fn table_passed(&self, end: usize) -> bool {
        self.machine.utf16_offset() >= end as u64
    }
}
