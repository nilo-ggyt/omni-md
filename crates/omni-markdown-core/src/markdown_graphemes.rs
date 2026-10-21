//! Paged Unicode 18 extended-grapheme indexing borrows the original frozen pieces without
//! creating another text file or an alternate encoding of the document.
//!
//! This module supplies original Source byte/UTF-16 boundaries, not soft lines, fonts, or Native
//! geometry. Classified prefixes are queryable; unknown successors stay Pending and only actual
//! source EOF completes scanning. `paged-graphemes` does not select a Native rendering path.

use thiserror::Error;

use crate::{
    MARKDOWN_LAYOUT_SOURCE_BYTES, MarkdownDocumentBufferError, MarkdownLayoutTextAuthority,
    MarkdownLayoutTextSource, MarkdownLayoutTextSourceObserver,
};

mod machine;
#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod table_rows;
#[cfg(feature = "paged-shaping-frontier")]
pub use prefix::{MarkdownGraphemePrefix, MarkdownGraphemePrefixFacts, MarkdownGraphemePrefixRead};
mod store;
mod unicode_data;

#[cfg(feature = "paged-scripts")]
pub(crate) use machine::ScalarCursor as MarkdownGraphemeScalarCursor;

/// Hard limit for each source borrow, including when an edited piece is larger than this limit.
pub const MARKDOWN_GRAPHEME_SOURCE_BYTES: usize = MARKDOWN_LAYOUT_SOURCE_BYTES;
/// One index retains one bounded metadata page; document growth does not add resident index pages.
pub const MARKDOWN_GRAPHEME_INDEX_BYTES: usize =
    crate::markdown_layout_index::MARKDOWN_LAYOUT_INDEX_BYTES;

#[derive(Debug, Error)]
pub enum MarkdownGraphemeError {
    #[error("字素扫描配额无效")]
    InvalidBudget,
    #[error("字素查询范围无效")]
    InvalidRange,
    #[error("字素查询位于显式折叠的隐藏内部")]
    FoldedRange,
    #[error("字素查询位于独立表格行内部，须使用该格显示来源")]
    TableRange,
    #[error("字素源权限不匹配")]
    SourceMismatch,
    #[error("字素坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结正文不是完整UTF-8")]
    InvalidUtf8,
    #[error("字素索引元数据不完整")]
    CorruptMetadata,
    #[error("字素任务已因失败或取消永久撤权")]
    Poisoned,
    #[error("无法借用冻结正文")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名字素索引")]
    Storage(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, MarkdownGraphemeError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGraphemeBoundary {
    pub byte: usize,
    pub utf16: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeQuery {
    IsBoundary,
    Previous,
    Next,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeAnswer {
    Pending,
    Known(Option<MarkdownGraphemeBoundary>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeStatus {
    Pending,
    Ready,
}

/// Actual record domain for ordinals; every domain retains the complete original Source coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownGraphemeOrdinalDomain {
    Source,
    /// Visible boundaries and fold endpoints only; adjacent ordinals may enclose unscanned hidden text.
    FoldedVisible,
    /// Table interiors grant no body GCB authority; ordinals cover body text and structural row endpoints.
    TableVisible,
}

/// Three independent work dimensions; a zero quantum is invalid rather than an endlessly retryable Pending.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownGraphemeBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
}
impl Default for MarkdownGraphemeBudget {
    fn default() -> Self {
        Self {
            bytes: MARKDOWN_GRAPHEME_SOURCE_BYTES,
            scalars: 1024,
            records: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownGraphemeStep {
    pub status: MarkdownGraphemeStatus,
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct MarkdownGraphemeStorageProfile {
    pub generation: u64,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    /// Proven logical boundaries in the current record domain; Dense allocates no row per boundary.
    pub boundary_count: usize,
    /// Actual 16-byte anonymous-index rows; always zero for Dense.
    pub boundary_records: usize,
    pub dense_printable_ascii: bool,
    /// Successful physical record reads/writes, excluding Dense coordinate arithmetic.
    pub index_records_read: u64,
    pub index_records_written: u64,
    pub index_page_loads: u64,
    pub index_peak_mapped_bytes: usize,
    pub unicode_version: (u8, u8, u8),
}

/// Fixed atomic observations outlive the job without retaining its text, file, or mapping owners.
#[derive(Clone)]
pub struct MarkdownGraphemeResourceObserver {
    index: store::BoundaryResourceObserver,
    source_dropped: MarkdownLayoutTextSourceObserver,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownGraphemeResourceSnapshot {
    /// Paged storage owner was allocated; false for Dense cannot prove any real mapping was released.
    pub metadata_storage_allocated: bool,
    /// Paged resources were returned, or the Dense owner actually closed/dropped; neither retains Source.
    pub metadata_owner_released: bool,
    pub metadata_file_open: bool,
    pub metadata_map_bytes: usize,
    pub metadata_peak_map_bytes: usize,
    /// Current/peak heap bytes of the original metadata write buffer, separate from source mappings.
    pub metadata_buffer_bytes: usize,
    pub metadata_peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    /// This source payload dropped its Snapshot field; other snapshots or Native values are independent.
    pub source_payload_released: bool,
}
impl MarkdownGraphemeResourceObserver {
    #[must_use]
    pub fn snapshot(&self) -> MarkdownGraphemeResourceSnapshot {
        let index = self.index.snapshot();
        MarkdownGraphemeResourceSnapshot {
            metadata_storage_allocated: index.allocated,
            metadata_owner_released: index.released,
            metadata_file_open: index.file_open,
            metadata_map_bytes: index.mapped_bytes,
            metadata_peak_map_bytes: index.peak_mapped_bytes,
            metadata_buffer_bytes: index.buffer_bytes,
            metadata_peak_buffer_bytes: index.peak_buffer_bytes,
            cleanup_failed: index.cleanup_failed,
            source_payload_released: self.source_dropped.payload_released(),
        }
    }
}

/// One original Source and one closed boundary representation: Unicode uses a streaming machine
/// and anonymous index, while Dense requires exact whole-source proof. An empty index does not
/// certify unknown text; even proven Dense results remain subject to source health and revocation.
pub struct MarkdownGraphemeJob {
    // Drop the index's actual storage before this source payload; observers retain only fixed metadata.
    index: store::BoundaryIndex,
    source: MarkdownLayoutTextSource,
    machine: machine::Machine,
    failed: bool,
    // Global cursors may jump over explicit hidden regions; count only actually dereferenced bytes as reads.
    source_bytes_read: usize,
    source_borrow_max: usize,
    #[cfg(feature = "paged-line-compose")]
    folds: Option<FoldCursor>,
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    tables: Option<table_rows::State>,
    #[cfg(test)]
    split: Option<usize>,
}

/// Algorithms share the same endpoint proof; this job retains only its own cursor, not copied spans or text.
#[cfg(feature = "paged-line-compose")]
struct FoldCursor {
    plan: crate::markdown_layout_source::folds::FoldPlan,
    next: usize,
    tail: bool,
}

impl MarkdownGraphemeJob {
    /// Exact printable-ASCII proof of this entire frozen source authorizes integer GCB positions;
    /// other sources use the original paged owner. Admission neither scans text nor infers its
    /// character domain from matching generations, samples, or byte lengths.
    ///
    /// # Errors
    /// Preserve typed Source, coordinate, or storage failures; local owners drop on failed construction.
    pub fn new(source: MarkdownLayoutTextSource) -> Result<Self> {
        source.health()?;
        #[cfg(feature = "paged-shaping-frontier")]
        if source.is_printable_ascii_range(0..source.utf16_len())? {
            if source.byte_len() != source.utf16_len() {
                return Err(MarkdownGraphemeError::CorruptMetadata);
            }
            let index = store::BoundaryIndex::dense_ascii(source.byte_len())?;
            return Self::with_index(source, index);
        } // Query the complete original Snapshot aggregate, without reading partial leaves or minting another proof.
        Self::with_index(source, store::BoundaryIndex::new()?)
    }

    fn with_index(source: MarkdownLayoutTextSource, index: store::BoundaryIndex) -> Result<Self> {
        let mut machine = machine::Machine::default();
        if index.is_dense() {
            // Exact whole-source proof already establishes EOF. Set logical progress without
            // claiming that the job read any of those source bytes.
            machine.byte = source.byte_len();
            machine.utf16 = source.utf16_len();
            machine.frontier = Some(MarkdownGraphemeBoundary {
                byte: machine.byte,
                utf16: machine.utf16,
            });
            machine.ready = true;
        }
        source.health()?;
        Ok(Self {
            index,
            source,
            machine,
            failed: false,
            source_bytes_read: 0,
            source_borrow_max: 0,
            #[cfg(feature = "paged-line-compose")]
            folds: None,
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            tables: None,
            #[cfg(test)]
            split: None,
        })
    }

    /// Install same-Source folds before scanning; ordinary jobs retain their representation and ordinals.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn install_fold_plan(
        &mut self,
        plan: crate::markdown_layout_source::folds::FoldPlan,
    ) -> Result<()> {
        self.guard(plan.authority())?;
        if plan.spans().is_empty() {
            return Ok(());
        } // Empty display intent does not replace Dense or create a second initialization path.
        if self.machine.byte != 0 || self.machine.ready || self.index.len() != 0 {
            self.failed = true;
            return Err(MarkdownGraphemeError::InvalidRange);
        }
        self.folds = Some(FoldCursor {
            plan,
            next: 0,
            tail: false,
        });
        self.source.health().inspect_err(|_| self.failed = true)?;
        Ok(())
    }

    /// Counts and prefix reads use this domain; visible domains do not supply whole-source GCB ordinals.
    #[must_use]
    pub(crate) fn ordinal_domain(&self) -> MarkdownGraphemeOrdinalDomain {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if self.tables.is_some() {
            return MarkdownGraphemeOrdinalDomain::TableVisible;
        }
        #[cfg(feature = "paged-line-compose")]
        if self.folds.is_some() {
            return MarkdownGraphemeOrdinalDomain::FoldedVisible;
        }
        MarkdownGraphemeOrdinalDomain::Source
    }

    /// Streaming and real-FD failure oracles explicitly select Paged so Dense cannot bypass their I/O.
    #[cfg(test)]
    fn new_paged(source: MarkdownLayoutTextSource) -> Result<Self> {
        source.health()?;
        Self::with_index(source, store::BoundaryIndex::new()?)
    }

    fn guard(&mut self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        if let Err(error) = self.source.health() {
            self.failed = true;
            return Err(error.into()); // Another reader's original Source failure also invalidates cached Ready/metadata.
        }
        if self.failed {
            return Err(MarkdownGraphemeError::Poisoned);
        }
        if !self.source.permits(authority) {
            // Foreign authority permanently revokes this job; recapturing newer text cannot
            // reauthorize the existing index.
            self.failed = true;
            return Err(MarkdownGraphemeError::SourceMismatch);
        }
        Ok(())
    }

    /// Advance the original source through bounded, nonescaping borrows without a document String.
    /// At most one four-byte UTF-8 scalar remains pending across pieces or steps.
    ///
    /// # Errors
    /// Authority, quantum, text, or I/O failures permanently revoke successful queries, even with cached pages.
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownGraphemeBudget,
    ) -> Result<MarkdownGraphemeStep> {
        self.guard(authority)?;
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 {
            self.failed = true;
            return Err(MarkdownGraphemeError::InvalidBudget);
        }
        let result = self.advance(budget);
        // A real Source failure discovered during the borrow takes precedence over a quota
        // stop or local machine result; neither can publish stale successful coverage.
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn advance(&mut self, budget: MarkdownGraphemeBudget) -> Result<MarkdownGraphemeStep> {
        let mut step = MarkdownGraphemeStep {
            status: MarkdownGraphemeStatus::Pending,
            bytes: 0,
            scalars: 0,
            records: 0,
        };
        if self.machine.ready {
            step.status = MarkdownGraphemeStatus::Ready;
            return Ok(step);
        }
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if table_rows::advance(self, budget, &mut step)? {
            return Ok(step);
        }
        let source = &self.source;
        let start = self.machine.byte;
        let end = start
            .saturating_add(budget.bytes.min(MARKDOWN_GRAPHEME_SOURCE_BYTES))
            .min(self.source.byte_len());
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        let end = table_rows::read_end(self, end);
        #[cfg(feature = "paged-line-compose")]
        let fold = self
            .folds
            .as_ref()
            .and_then(|cursor| cursor.plan.spans().get(cursor.next).copied())
            .map(|span| (span, self.folds.as_ref().is_some_and(|cursor| cursor.tail)));
        #[cfg(feature = "paged-line-compose")]
        let end = fold.map_or(end, |(span, tail)| {
            end.min(if tail {
                span.end.byte
            } else if start < span.start.byte {
                span.start.byte
            } else {
                span.header_end.byte
            })
        }); // End each borrow at the actual control edge; hidden body bytes never enter the visitor.
        #[cfg(test)]
        let end = self
            .split
            .filter(|&split| split > start)
            .map_or(end, |split| end.min(split));
        // A borrow may end inside UTF-8. Only Machine owns continuous decoding state;
        // ending this visit does not establish source EOF.
        let mut failure = None;
        let mut stopped = false;
        let result = source.visit_byte_range(start..end, |piece| {
            self.source_borrow_max = self.source_borrow_max.max(piece.len());
            for byte in piece {
                // Iteration yields a reference; check quotas before dereferencing the next byte
                // so an unread byte is never consumed without being charged.
                if step.scalars == budget.scalars || step.records == budget.records {
                    // This visitor error is a local stop signal, distinguished below from real Source errors.
                    stopped = true;
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                self.source_bytes_read += 1;
                #[cfg(feature = "paged-line-compose")]
                let pushed = if fold.is_some_and(|(_, tail)| tail) {
                    self.machine
                        .push_fold_tail(*byte, &mut self.index, &mut step)
                } else {
                    self.machine.push(*byte, &mut self.index, &mut step)
                };
                #[cfg(not(feature = "paged-line-compose"))]
                let pushed = self.machine.push(*byte, &mut self.index, &mut step);
                if let Err(error) = pushed {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {}
            // Only this callback's quota stop or machine failure may consume the sentinel;
            // actual source-reclaim failures propagate instead.
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
                if stopped || failure.is_some() => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(error) = failure {
            return Err(error);
        }
        #[cfg(feature = "paged-line-compose")]
        if let Some((span, tail)) = fold {
            if tail && self.machine.byte == span.end.byte {
                if self.machine.utf16 != span.end.utf16 || self.machine.used != 0 {
                    return Err(MarkdownGraphemeError::CorruptMetadata);
                }
                let cursor = self
                    .folds
                    .as_mut()
                    .expect("原 fold cursor 仍由本次推进持有");
                cursor.next += 1;
                cursor.tail = false;
            } else if !tail && self.machine.byte == span.header_end.byte {
                // The original DFA consumed the complete opening CRLF. Jump to the actual
                // closing control bytes without decoding the hidden body.
                let target = if span.header_end == span.end {
                    span.end
                } else {
                    span.tail_start
                };
                self.machine
                    .jump_fold(span.header_end, target, span.start)?;
                let cursor = self
                    .folds
                    .as_mut()
                    .expect("原 fold cursor 仍由本次推进持有");
                if target == span.end {
                    cursor.next += 1;
                    cursor.tail = false;
                } else {
                    cursor.tail = true;
                }
            }
        }
        if self.machine.byte == self.source.byte_len() {
            // Finish only at immutable Source EOF with matching UTF-16 aggregate and decoded
            // scalars. A short visit cannot substitute for end-of-text authority.
            if self.machine.used != 0 || self.machine.utf16 != self.source.utf16_len() {
                return Err(MarkdownGraphemeError::InvalidUtf8);
            }
            if step.records < budget.records {
                self.machine.finish(&mut self.index, &mut step)?;
            }
        }
        Ok(step)
    }

    /// Query published boundaries in the current domain; Previous/Next are strict neighbors.
    /// Ordinary queries may inspect UTF-8 interiors or low-surrogate coordinates, while hidden
    /// folded interiors are explicitly rejected with FoldedRange.
    ///
    /// # Errors
    /// Range, authority, and metadata failures revoke the job. FoldedRange/TableRange preserve
    /// a healthy job for later queries in its visible domain.
    pub fn query(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownGraphemeUnit,
        offset: usize,
        query: MarkdownGraphemeQuery,
    ) -> Result<MarkdownGraphemeAnswer> {
        self.guard(authority)?;
        let result = self.query_inner(unit, offset, query);
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        let result = result.and_then(|answer| {
            if let (MarkdownGraphemeAnswer::Known(Some(value)), Some(state)) =
                (&answer, &self.tables)
                && state.cursor.plan.contains(value.utf16, true)?
            {
                return Err(MarkdownGraphemeError::TableRange);
            }
            Ok(answer)
        });
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if result.as_ref().is_err_and(|error| {
            !matches!(
                error,
                MarkdownGraphemeError::FoldedRange | MarkdownGraphemeError::TableRange
            )
        }) {
            self.failed = true;
        }
        result
    }

    /// Supply the current domain's count only after actual Ready; interpret it with ordinal_domain.
    /// None means unknown, not zero boundaries or a whole-source total.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn ready_boundary_count(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
    ) -> Result<Option<usize>> {
        // Original Source I/O failure precedes an old failed bit; prior cancellation or
        // invalid input cannot hide a later actual mapping failure.
        self.source.health().inspect_err(|_| self.failed = true)?;
        self.guard(authority)?;
        let count = self.machine.ready.then_some(self.index.len());
        self.source.health().inspect_err(|_| self.failed = true)?;
        Ok(count)
    }

    /// Export at most 256 completed original GCB records without exposing page borrows or source owners.
    #[cfg(feature = "paged-line-compose")]
    pub(crate) fn read_ready_boundaries(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        range: std::ops::Range<usize>,
        output: &mut [MarkdownGraphemeBoundary],
    ) -> Result<bool> {
        let result = (|| {
            self.source.health()?;
            self.guard(authority)?;
            if range.end.checked_sub(range.start) != Some(output.len()) || output.len() > 256 {
                return Err(MarkdownGraphemeError::InvalidRange);
            }
            if !self.machine.ready {
                return Ok(false);
            } // Pending leaves output untouched and does not promote a known prefix to a complete table.
            self.index.read_batch(range, output)?;
            self.guard(authority)?;
            Ok(true)
        })();
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if result.is_err() {
            self.failed = true;
        } // I/O, invalid input, or foreign authority cannot revive Ready or publish a partially written batch.
        result
    }

    fn query_inner(
        &mut self,
        unit: MarkdownGraphemeUnit,
        offset: usize,
        query: MarkdownGraphemeQuery,
    ) -> Result<MarkdownGraphemeAnswer> {
        let position = |b: MarkdownGraphemeBoundary| match unit {
            MarkdownGraphemeUnit::Byte => b.byte,
            MarkdownGraphemeUnit::Utf16 => b.utf16,
        };
        let maximum = match unit {
            MarkdownGraphemeUnit::Byte => self.source.byte_len(),
            MarkdownGraphemeUnit::Utf16 => self.source.utf16_len(),
        };
        if offset > maximum {
            return Err(MarkdownGraphemeError::InvalidRange);
        }
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if let Some(tables) = &self.tables
            && tables
                .cursor
                .plan
                .contains(offset, unit == MarkdownGraphemeUnit::Utf16)?
        {
            return Err(MarkdownGraphemeError::TableRange);
        }
        #[cfg(feature = "paged-line-compose")]
        if self.folds.as_ref().is_some_and(|cursor| {
            cursor
                .plan
                .hides(offset, unit == MarkdownGraphemeUnit::Utf16)
        }) {
            return Err(MarkdownGraphemeError::FoldedRange);
        } // A hidden interior is outside this display domain, not a fabricated missing boundary or a poisoned job.
        if self.index.is_dense() {
            // Exact original proof makes every integer position a GCB. Strict neighbors use
            // checked arithmetic, while outer guards still check Source health and authority.
            let ordinal = match query {
                MarkdownGraphemeQuery::IsBoundary => Some(offset),
                MarkdownGraphemeQuery::Previous => offset.checked_sub(1),
                MarkdownGraphemeQuery::Next if offset == maximum => None,
                MarkdownGraphemeQuery::Next => Some(
                    offset
                        .checked_add(1)
                        .ok_or(MarkdownGraphemeError::CoordinateOverflow)?,
                ),
            };
            return Ok(MarkdownGraphemeAnswer::Known(ordinal.map(|position| {
                MarkdownGraphemeBoundary {
                    byte: position,
                    utf16: position,
                }
            })));
        }
        let known = self.machine.frontier.is_some_and(|b| position(b) >= offset);
        if !known
            && matches!(
                query,
                MarkdownGraphemeQuery::IsBoundary | MarkdownGraphemeQuery::Previous
            )
        {
            // The stable frontier has not classified this position. Avoid searching old records
            // for an inevitably Pending answer; outer health and visible-domain checks still apply.
            return Ok(MarkdownGraphemeAnswer::Pending);
        }
        let mut low = 0;
        let mut high = self.index.len();
        // Retain only the boundaries actually read in this binary search, not a cross-query
        // cache. On exit they are the strict predecessor and lower bound, avoiding duplicate reads.
        let mut previous = None;
        let mut following = None;
        if let Some(page) = self.index.append_page_records()?
            && !page.is_empty()
        {
            // The current frontier commonly lies in the original 16 KiB append page. Its first
            // real record narrows the search to that resident range or the older original FD;
            // no older page is prefetched and no additional records or Source owners are retained.
            let first = self.index.get(page.start)?;
            if position(first) < offset {
                low = page.start + 1;
                high = page.end;
                previous = Some(first);
            } else {
                high = page.start;
                following = Some(first); // The append page's first record may be the strict successor of an older position.
                if position(first) == offset {
                    low = page.start;
                    if matches!(query, MarkdownGraphemeQuery::Previous) && page.start != 0 {
                        // A page's first record is not its own strict predecessor; read the actual previous record.
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
            MarkdownGraphemeQuery::IsBoundary => Ok(MarkdownGraphemeAnswer::Known(
                following.filter(|boundary| position(*boundary) == offset),
            )),
            MarkdownGraphemeQuery::Previous => Ok(MarkdownGraphemeAnswer::Known(previous)),
            MarkdownGraphemeQuery::Next => {
                if let Some(boundary) = following {
                    if position(boundary) != offset {
                        return Ok(MarkdownGraphemeAnswer::Known(Some(boundary)));
                    }
                    // Next is strict: only an equal lower bound requires reading the following record.
                    low += 1;
                }
                if low < self.index.len() {
                    return Ok(MarkdownGraphemeAnswer::Known(Some(self.index.get(low)?)));
                }
                Ok(if self.machine.ready {
                    MarkdownGraphemeAnswer::Known(None)
                } else {
                    MarkdownGraphemeAnswer::Pending
                })
            }
        }
    }

    /// Revoke authority without claiming to free immutable values held by other clones; Drop/close owns release.
    pub fn cancel(&mut self) {
        self.failed = true;
    }
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.failed && self.machine.ready && self.source.health().is_ok()
    }
    #[must_use]
    pub fn storage_profile(&self) -> MarkdownGraphemeStorageProfile {
        MarkdownGraphemeStorageProfile {
            generation: self.source.generation(),
            source_bytes: self.source.byte_len(),
            source_utf16: self.source.utf16_len(),
            source_bytes_read: self.source_bytes_read,
            source_borrow_max_bytes: self.source_borrow_max,
            boundary_count: self.index.len(),
            boundary_records: self.index.physical_records(),
            dense_printable_ascii: self.index.is_dense(),
            index_records_read: self.index.io_counts().0,
            index_records_written: self.index.io_counts().1,
            index_page_loads: self.index.page_loads(),
            index_peak_mapped_bytes: self.index.resource_observer().snapshot().peak_mapped_bytes,
            unicode_version: (18, 0, 0),
        }
    }
    #[must_use]
    pub fn resource_observer(&self) -> MarkdownGraphemeResourceObserver {
        MarkdownGraphemeResourceObserver {
            index: self.index.resource_observer(),
            source_dropped: self.source.resource_observer(),
        }
    }

    /// Explicitly return this index's actual storage. Failure still attempts cleanup without certifying success.
    ///
    /// # Errors
    /// Storage cleanup preserves its failure facts; an original Source failure takes return-value
    /// precedence. External source clones retain their own independent lifetimes.
    pub fn close(mut self) -> Result<()> {
        let result = self.index.close();
        self.source.health()?; // Return actual index resources first, then preserve an existing Source failure.
        result
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-line-compose",
    feature = "paged-shaping-frontier"
))]
impl MarkdownGraphemeJob {
    /// Structural passage requires the machine's actual progress and a committed endpoint, not a requested jump.
    pub(crate) fn table_passed(&self, end: usize) -> bool {
        self.machine.utf16 >= end
            && self
                .machine
                .last_committed_boundary()
                .is_some_and(|value| value.utf16 >= end)
    }
}
