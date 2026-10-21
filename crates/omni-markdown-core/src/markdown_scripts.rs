//! Bounded Unicode 18 script itemization; a source window is not a paragraph,
//! grapheme, or script boundary.
//!
//! This policy takes the full SCX of a cluster's first specific base, resolves
//! Common/Inherited through context, and gives actual paired brackets the outer
//! script. Remaining candidates prefer a compatible preceding run, then the
//! primary script, then stable ISO tag order. Unicode does not mandate this one
//! itemization policy. Downstream composition intersects fonts, UBA levels, and
//! width; this stage does not infer those boundaries.
use crate::{
    MarkdownDocumentBufferError, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownLayoutTextSourceObserver, markdown_layout_index::LayoutIndexObserver,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use thiserror::Error;

#[cfg(feature = "paged-line-compose")]
mod folds;
mod machine;
#[cfg(feature = "paged-shaping-frontier")]
mod prefix;
#[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
mod table_rows;
#[cfg(feature = "paged-shaping-frontier")]
pub use prefix::{
    MarkdownScriptPrefixAnswer, MarkdownScriptPrefixKind, MarkdownStableScriptPrefix,
};
#[cfg(feature = "paged-shaping-frontier")]
mod printable;
mod properties;
mod store;
#[cfg(test)]
mod tests;
mod unicode_data;

pub const MARKDOWN_SCRIPT_SOURCE_BYTES: usize = 4096;
pub const MARKDOWN_SCRIPT_INDEX_BYTES: usize =
    2 * crate::markdown_layout_index::MARKDOWN_LAYOUT_INDEX_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownScript(u8);
impl MarkdownScript {
    /// Exposes the ISO 15924 tag for a shaper's public tag constructor without
    /// making the generated table's internal ordinal part of that API.
    pub fn iso15924_tag(self) -> [u8; 4] {
        unicode_data::TAGS[usize::from(self.0)]
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownScriptRun {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_utf16: usize,
    pub end_utf16: usize,
    pub script: MarkdownScript,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptUnit {
    Byte,
    Utf16,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptAnswer {
    Pending,
    Known(Option<MarkdownScriptRun>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownScriptStatus {
    Pending,
    Ready,
}

#[derive(Clone, Default)]
pub struct MarkdownScriptCancellation(Arc<AtomicBool>);
impl MarkdownScriptCancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(MarkdownScriptError::Cancelled)
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Error)]
pub enum MarkdownScriptError {
    #[error("脚本扫描配额无效")]
    InvalidBudget,
    #[error("脚本查询范围无效")]
    InvalidRange,
    #[cfg(feature = "paged-line-compose")]
    #[error("该原 Source 范围已折叠，没有可见脚本权限")]
    FoldedRange,
    #[error("独立表格行内部须查询该格显示来源的脚本")]
    TableRange,
    #[error("脚本源权限不匹配")]
    SourceMismatch,
    #[error("脚本坐标或计数溢出")]
    CoordinateOverflow,
    #[error("冻结脚本源不是完整UTF8")]
    InvalidUtf8,
    #[error("脚本索引元数据不完整")]
    CorruptMetadata,
    #[error("脚本任务已取消")]
    Cancelled,
    #[error("脚本任务已永久撤权")]
    Poisoned,
    #[error("无法读取冻结脚本源")]
    Source(#[from] MarkdownDocumentBufferError),
    #[error("无法访问匿名脚本索引")]
    Storage(#[from] std::io::Error),
}
type Result<T> = std::result::Result<T, MarkdownScriptError>;

/// Source bytes, decoded scalars, record writes, and fixed metadata operations
/// have separate quotas. Deep bracket stacks resume across steps instead of
/// silently truncating nesting at an arbitrary depth.
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptBudget {
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub stack_ops: usize,
}
impl Default for MarkdownScriptBudget {
    fn default() -> Self {
        Self {
            bytes: 4096,
            scalars: 1024,
            records: 1024,
            stack_ops: 1024,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptStep {
    pub status: MarkdownScriptStatus,
    pub bytes: usize,
    pub scalars: usize,
    pub records: usize,
    pub stack_ops: usize,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptStorageProfile {
    pub generation: u64,
    pub source_bytes: usize,
    pub source_utf16: usize,
    pub source_bytes_read: usize,
    pub source_borrow_max_bytes: usize,
    /// Identifies the exact original-source summary path. Skipped source bytes
    /// are not reported as reads in `source_bytes_read`.
    pub printable_summary_used: bool,
    pub completed_runs: usize,
    pub bracket_records: usize,
    pub run_page_loads: u64,
    pub stack_page_loads: u64,
    pub unicode_version: (u8, u8, u8),
}
#[derive(Clone)]
pub struct MarkdownScriptResourceObserver {
    runs: LayoutIndexObserver,
    frames: LayoutIndexObserver,
    source: MarkdownLayoutTextSourceObserver,
}
#[derive(Clone, Copy, Debug)]
pub struct MarkdownScriptResourceSnapshot {
    pub run_file_open: bool,
    pub stack_file_open: bool,
    pub mapped_bytes: usize,
    pub run_peak_mapped_bytes: usize,
    pub stack_peak_mapped_bytes: usize,
    /// Current working buffers of the two independent indexes. Their individual
    /// peaks do not establish a simultaneous peak or a process RSS measurement.
    pub buffer_bytes: usize,
    pub run_peak_buffer_bytes: usize,
    pub stack_peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    pub source_payload_released: bool,
}
impl MarkdownScriptResourceObserver {
    pub fn snapshot(&self) -> MarkdownScriptResourceSnapshot {
        let runs = self.runs.snapshot();
        let frames = self.frames.snapshot();
        MarkdownScriptResourceSnapshot {
            run_file_open: runs.file_open,
            stack_file_open: frames.file_open,
            mapped_bytes: runs.mapped_bytes + frames.mapped_bytes,
            run_peak_mapped_bytes: runs.peak_mapped_bytes,
            stack_peak_mapped_bytes: frames.peak_mapped_bytes,
            buffer_bytes: runs.buffer_bytes + frames.buffer_bytes,
            run_peak_buffer_bytes: runs.peak_buffer_bytes,
            stack_peak_buffer_bytes: frames.peak_buffer_bytes,
            cleanup_failed: runs.cleanup_failed || frames.cleanup_failed,
            source_payload_released: self.source.payload_released(),
        }
    }
}

/// Owns the frozen source and both metadata indexes. The retained authority
/// identifies this capture; every public operation rechecks its permission and
/// the source's sticky health before publishing cached or newly computed facts.
pub struct MarkdownScriptJob {
    store: store::Store,
    machine: machine::Machine,
    source: MarkdownLayoutTextSource,
    authority: MarkdownLayoutTextAuthority,
    cancellation: MarkdownScriptCancellation,
    failed: bool,
    cancelled: bool,
    source_borrow_max: usize,
    source_bytes_read: usize,
    #[cfg(feature = "paged-line-compose")]
    folds: Option<folds::State>,
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    tables: Option<table_rows::State>,
    #[cfg(feature = "paged-shaping-frontier")]
    completed_ascii_suffix: Option<(std::ops::Range<usize>, bool)>,
    #[cfg(feature = "paged-shaping-frontier")]
    printable: Option<printable::Preparation>,
}
impl MarkdownScriptJob {
    /// Binds the original source and weak authority and creates bounded metadata
    /// owners. Construction does not scan the body.
    pub fn new(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownScriptCancellation,
    ) -> Result<Self> {
        Self::create(source, authority, cancellation, true)
    }

    /// Keeps the original scalar machine available for differential and failure
    /// tests; the optimized path must not replace its own semantic oracle.
    #[cfg(test)]
    fn new_streaming(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownScriptCancellation,
    ) -> Result<Self> {
        Self::create(source, authority, cancellation, false)
    }

    fn create(
        source: MarkdownLayoutTextSource,
        authority: &MarkdownLayoutTextAuthority,
        cancellation: MarkdownScriptCancellation,
        use_summary: bool,
    ) -> Result<Self> {
        source.health()?;
        if !source.permits(authority) {
            return Err(MarkdownScriptError::SourceMismatch);
        }
        cancellation.check()?; // Reject cancellation or foreign identity before creating anonymous FDs.
        #[cfg(feature = "paged-shaping-frontier")]
        let printable = if use_summary {
            // Exact facts of the original full root need no body lease. Without
            // that proof the complete original machine remains responsible.
            printable::Preparation::capture(&source)?
        } else {
            None // Only the test constructor disables the production summary choice.
        };
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let _ = use_summary; // Without the frontier feature, both constructors use scalar scanning.
        source.health()?;
        cancellation.check()?;
        let store = store::Store::new(&cancellation);
        source.health()?;
        let store = store?;
        cancellation.check()?; // Source failure wins during construction; cancelled candidates are not returned live.
        Ok(Self {
            store,
            machine: machine::Machine::default(),
            source,
            authority: authority.clone(),
            cancellation,
            failed: false,
            cancelled: false,
            source_borrow_max: 0,
            source_bytes_read: 0,
            #[cfg(feature = "paged-line-compose")]
            folds: None,
            #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
            tables: None,
            #[cfg(feature = "paged-shaping-frontier")]
            completed_ascii_suffix: None,
            #[cfg(feature = "paged-shaping-frontier")]
            printable,
        })
    }
    fn guard(&self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        self.source.health()?; // The source's real errno precedes metadata or cancellation, even on a cache hit.
        if self.cancelled {
            return Err(MarkdownScriptError::Cancelled);
        }
        if self.failed {
            return Err(MarkdownScriptError::Poisoned);
        }
        if authority != &self.authority || !self.source.permits(authority) {
            return Err(MarkdownScriptError::SourceMismatch);
        }
        self.cancellation.check()
    }
    fn fail(&mut self, error: &MarkdownScriptError) {
        // A table query needs its cell's display source; that local range refusal
        // does not invalidate already authorized runs outside the table.
        if matches!(error, MarkdownScriptError::TableRange) {
            return;
        }
        self.failed = true;
        self.cancelled = matches!(error, MarkdownScriptError::Cancelled);
        self.store.poison();
    }
    pub fn step(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        budget: MarkdownScriptBudget,
    ) -> Result<MarkdownScriptStep> {
        if let Err(error) = self.guard(authority) {
            self.fail(&error);
            return Err(error);
        }
        if budget.bytes == 0 || budget.scalars == 0 || budget.records == 0 || budget.stack_ops == 0
        {
            // A zero quota is a caller error, not a Pending result that can ask
            // the caller to retry forever without making progress.
            self.fail(&MarkdownScriptError::InvalidBudget);
            return Err(MarkdownScriptError::InvalidBudget);
        }
        let result = self.advance(budget);
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result.and_then(|step| {
                self.guard(authority)?;
                Ok(step)
            }),
        }; // Recheck source health even on metadata failure or cancellation; preserve its first known errno.
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }
    fn advance(&mut self, budget: MarkdownScriptBudget) -> Result<MarkdownScriptStep> {
        let mut step = MarkdownScriptStep {
            status: MarkdownScriptStatus::Pending,
            bytes: 0,
            scalars: 0,
            records: 0,
            stack_ops: 0,
        };
        if self.machine.ready {
            step.status = MarkdownScriptStatus::Ready;
            return Ok(step);
        }
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if table_rows::advance(self, budget, &mut step)? {
            self.source_bytes_read += step.bytes;
            return Ok(step);
        }
        #[cfg(feature = "paged-line-compose")]
        if folds::advance_boundary(self, budget, &mut step)? {
            self.source_bytes_read += step.bytes;
            return Ok(step);
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if let Some(preparation) = &mut self.printable {
            // The same preparation owns a whole-root summary or a contextual
            // suffix proof. Boundary-leaf reads and directory work consume this
            // quantum's quotas. Aggregate skips are not reads, and Ready requires
            // the actual store to finish sealing.
            let result = preparation.advance(
                &self.source,
                &mut self.source_borrow_max,
                &mut self.store,
                &mut step,
                budget,
            );
            self.source_bytes_read += step.bytes;
            match result? {
                printable::Progress::Ready => {
                    self.machine
                        .finish_proven_eof(self.source.byte_len(), self.source.utf16_len());
                    step.status = MarkdownScriptStatus::Ready;
                }
                printable::Progress::Rejected => {
                    self.completed_ascii_suffix = preparation.range().map(|range| (range, false));
                    self.printable = None;
                }
                printable::Progress::Resume(run) => {
                    self.machine
                        .advance_proven_ascii(run.end_byte, run.end_utf16);
                    self.completed_ascii_suffix = Some((run.start_utf16..run.end_utf16, true));
                    self.printable = None;
                } // Remain Pending: the original machine must still consume B/CRLF and close the run.
                printable::Progress::Pending => {}
            }
            return Ok(step);
        }
        // Finish suspended pairing or record work before borrowing more source;
        // metadata progress must not cause additional body reads.
        if !self
            .machine
            .pump(&mut self.store, &mut step, budget, false)?
        {
            return Ok(step);
        }
        let start = self.machine.byte;
        let end = start
            .saturating_add(budget.bytes.min(MARKDOWN_SCRIPT_SOURCE_BYTES))
            .min(self.source.byte_len());
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        let end = table_rows::read_end(self, end);
        #[cfg(feature = "paged-line-compose")]
        let end = folds::read_end(self, end);
        let mut paused = false;
        let mut failure = None;
        let source = &self.source;
        let result = source.visit_byte_range(start..end, |piece| {
            self.source_borrow_max = self.source_borrow_max.max(piece.len());
            for byte in piece {
                if let Err(error) = self.cancellation.check() {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                if step.scalars == budget.scalars
                    || step.records == budget.records
                    || step.stack_ops == budget.stack_ops
                {
                    paused = true;
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                if let Err(error) = self.machine.push_byte(*byte, &mut step) {
                    failure = Some(error);
                    return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                }
                match self.machine.pump(&mut self.store, &mut step, budget, false) {
                    Ok(true) => {}
                    Ok(false) => {
                        paused = true;
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                    }
                    Err(error) => {
                        failure = Some(error);
                        return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
                    }
                }
            }
            Ok(())
        });
        self.source_bytes_read += step.bytes;
        self.source.health()?; // The callback sentinel must not hide lease-return failure or another reader's revocation.
        match result {
            Ok(()) => {}
            Err(MarkdownDocumentBufferError::RangeOutsideDocument)
                if paused || failure.is_some() => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.cancellation.check()?;
        if self.machine.byte == self.source.byte_len() {
            // Only the original source endpoint permits EOF finalization. A
            // window edge cannot close an incomplete UTF-8 scalar or script run.
            if !self.machine.complete_utf8(self.source.utf16_len()) {
                return Err(MarkdownScriptError::InvalidUtf8);
            }
            self.machine
                .pump(&mut self.store, &mut step, budget, true)?;
        }
        if self.machine.ready {
            step.status = MarkdownScriptStatus::Ready;
        }
        Ok(step)
    }
    /// Requests proof only up to an original-source B content endpoint or EOF.
    /// A bounded preparation quantum does not establish a script boundary.
    #[cfg(feature = "paged-shaping-frontier")]
    pub(crate) fn prepare_ascii_suffix(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        position: usize,
        content_end: usize,
        paragraph_end: usize,
    ) -> Result<bool> {
        let result = (|| {
            self.guard(authority)?;
            if content_end > paragraph_end || paragraph_end > self.source.utf16_len() {
                return Err(MarkdownScriptError::InvalidRange);
            } // Reject inverted or out-of-source endpoints instead of clamping them into usable proof.
            if let Some(preparation) = &self.printable {
                return Ok(preparation
                    .range()
                    .is_some_and(|range| range.end == content_end && range.contains(&position)));
            } // An in-flight proof or seal can serve only its actual original range.
            if let Some((range, positive)) = &self.completed_ascii_suffix
                && range.end == content_end
                && range.contains(&position)
            {
                return Ok(*positive);
            } // Reuse a real counterexample or positive proof; repeated proof must not starve B consumption.
            if self.machine.ready {
                return Ok(false);
            }
            let Some(run) = self.machine.latin_suffix(position, content_end) else {
                return Ok(false);
            };
            if content_end == self.source.utf16_len()
                && self.source.byte_len() - run.start_byte != content_end - run.start_utf16
            {
                self.completed_ascii_suffix = Some((run.start_utf16..content_end, false));
                return Ok(false); // Only EOF supplies this byte extent; non-EOF proof must not synchronously read the paragraph.
            }
            self.printable = Some(printable::Preparation::suffix(
                &self.source,
                run,
                content_end,
            )?);
            self.guard(authority)?;
            Ok(true)
        })();
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result,
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    pub fn run_at(
        &mut self,
        authority: &MarkdownLayoutTextAuthority,
        unit: MarkdownScriptUnit,
        offset: usize,
    ) -> Result<MarkdownScriptAnswer> {
        if let Err(error) = self.guard(authority) {
            self.fail(&error);
            return Err(error);
        }
        #[cfg(feature = "paged-line-compose")]
        if folds::hidden(self, unit, offset) {
            return Err(MarkdownScriptError::FoldedRange);
        } // Source health was checked first; hidden-range refusal leaves valid visible runs authorized.
        let result = self.find_run(unit, offset);
        let result = match self.source.health() {
            Err(error) => Err(error.into()),
            Ok(()) => result.and_then(|answer| {
                self.guard(authority)?;
                Ok(answer)
            }),
        };
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }
    fn find_run(
        &mut self,
        unit: MarkdownScriptUnit,
        offset: usize,
    ) -> Result<MarkdownScriptAnswer> {
        #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
        if table_rows::hidden(self, unit, offset)? {
            return Err(MarkdownScriptError::TableRange);
        }
        #[cfg(feature = "paged-line-compose")]
        if folds::hidden(self, unit, offset) {
            return Err(MarkdownScriptError::FoldedRange);
        } // Stable-prefix queries must also respect hidden ranges, rather than assign following script to unanalyzed text.
        let end = match unit {
            MarkdownScriptUnit::Byte => self.source.byte_len(),
            MarkdownScriptUnit::Utf16 => self.source.utf16_len(),
        };
        if offset > end {
            return Err(MarkdownScriptError::InvalidRange);
        }
        // Search only contiguous completed runs. A current candidate with a
        // primary script is still not a published complete run.
        let position = |run: MarkdownScriptRun| match unit {
            MarkdownScriptUnit::Byte => run.end_byte,
            MarkdownScriptUnit::Utf16 => run.end_utf16,
        };
        let mut low = 0;
        let count = self.store.run_count();
        let mut high = count;
        let mut following = None;
        if let Some(page) = self.store.run_append_page()?
            && !page.is_empty()
        {
            // The append page's first complete run is already written. Its real
            // endpoint can narrow forward queries to initialized records. Older
            // records still use the original FD; neither capacity nor a pending
            // candidate supplies additional coverage.
            let first = self.store.run(page.start)?;
            if position(first) <= offset {
                low = page.start + 1;
                high = page.end;
            } else {
                high = page.start;
                following = Some(first); // If all older runs end before the query, this first run remains the answer.
            }
        }
        while low < high {
            let middle = low + (high - low) / 2;
            let run = self.store.run(middle)?;
            if position(run) <= offset {
                low = middle + 1;
            } else {
                high = middle;
                following = Some(run);
            }
        }
        if low < count {
            // This upper-bound search already decoded the answer. Reuse only
            // that stack-local Copy value, without rereading or retaining it
            // across queries. An equal endpoint still belongs to the next run
            // in both byte and UTF-16 coordinates.
            let run = following.ok_or(MarkdownScriptError::CorruptMetadata)?;
            return Ok(MarkdownScriptAnswer::Known(Some(run)));
        }
        // Unread successors and open runs remain Pending. Only actual EOF grants
        // Known(None); an empty published prefix is not proof of absence.
        Ok(if self.machine.ready {
            MarkdownScriptAnswer::Known(None)
        } else {
            MarkdownScriptAnswer::Pending
        })
    }
    pub fn storage_profile(&self) -> MarkdownScriptStorageProfile {
        let (run_page_loads, stack_page_loads) = self.store.loads();
        #[cfg(feature = "paged-shaping-frontier")]
        let printable_summary_used = self.printable.is_some();
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let printable_summary_used = false;
        MarkdownScriptStorageProfile {
            generation: self.source.generation(),
            source_bytes: self.source.byte_len(),
            source_utf16: self.source.utf16_len(),
            source_bytes_read: self.source_bytes_read,
            source_borrow_max_bytes: self.source_borrow_max,
            printable_summary_used,
            completed_runs: self.store.run_count(),
            bracket_records: self.store.frame_count(),
            run_page_loads,
            stack_page_loads,
            unicode_version: unicode_data::UNICODE_VERSION,
        }
    }
    pub fn resource_observer(&self) -> MarkdownScriptResourceObserver {
        let (runs, frames) = self.store.observers();
        MarkdownScriptResourceObserver {
            runs,
            frames,
            source: self.source.resource_observer(),
        }
    }
    pub fn close(mut self) -> Result<()> {
        // Cancellation does not skip cleanup. Both real index owners attempt
        // close; field destruction then relinquishes the original source.
        self.store.close()
    }
}

#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-line-compose",
    feature = "paged-shaping-frontier"
))]
impl MarkdownScriptJob {
    /// Resumes following text only after the actual SCX cursor reaches the row
    /// endpoint; cumulative bytes read do not establish that position.
    pub(crate) fn table_passed(&self, end: usize) -> bool {
        self.machine.utf16 >= end
    }
    /// A newly announced row invalidates a suffix attempt that might cross it.
    /// Subsequent work retains the same source and original step quotas.
    pub(crate) fn table_announced(&mut self) {
        self.printable = None;
        self.completed_ascii_suffix = None;
    }
}
