//! A read-only Source in display coordinates. Original piece bytes stay shared;
//! omissions and styles are represented only in the same anonymous paged index.

use std::sync::atomic::{AtomicI32, Ordering};

use super::*;
use crate::markdown_layout_index::{
    LayoutIndexError, LayoutIndexObserver, LayoutIndexReader, PagedLayoutIndex,
};
use crate::unicode_bidi_data::UnicodeBidiData;
use crate::{MarkdownPresentationStyles, MarkdownTableCellDisplayScalar};
use unicode_bidi::{BidiClass, BidiDataSource};

#[cfg(feature = "paged-shaping-frontier")]
pub(crate) mod algorithms;

const RECORD_BYTES: usize = 256;
const UNKNOWN_FAILURE: i32 = i32::MIN;

#[derive(Clone, Copy)]
/// The display start and actual UTF-8 end of a visible BMP terminator. Omitted
/// characters never contribute paragraph boundaries to these summaries.
pub(crate) struct DisplayProjectionTerminator {
    pub(crate) utf16: usize,
    pub(crate) byte_end: usize,
}

#[derive(Clone)]
/// One same-style run contiguous in both encodings and both coordinate spaces.
/// A record describes an actual original span; it stores no document text.
pub(crate) struct DisplayProjectionRun {
    pub(crate) display_bytes: Range<usize>,
    pub(crate) display_utf16: Range<usize>,
    pub(crate) raw_bytes: Range<usize>,
    pub(crate) raw_utf16: Range<usize>,
    pub(crate) styles: MarkdownPresentationStyles,
    pub(crate) non_printable_before: usize,
    pub(crate) non_printable_after: usize,
    pub(crate) first_bidi: Option<DisplayProjectionTerminator>,
    pub(crate) last_bidi: Option<DisplayProjectionTerminator>,
    pub(crate) first_text_terminator: Option<DisplayProjectionTerminator>,
    pub(crate) last_text_terminator: Option<DisplayProjectionTerminator>,
}

impl DisplayProjectionRun {
    /// Encode a fixed record containing only coordinates and summaries. Pointers,
    /// native values, document bytes, and mutable external objects never enter it.
    fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut record = [0; RECORD_BYTES];
        let mut put = |slot: usize, value: usize| {
            record[slot * 8..slot * 8 + 8].copy_from_slice(&(value as u64).to_le_bytes());
        };
        for (slot, value) in [
            self.display_bytes.start,
            self.display_bytes.end,
            self.display_utf16.start,
            self.display_utf16.end,
            self.raw_bytes.start,
            self.raw_bytes.end,
            self.raw_utf16.start,
            self.raw_utf16.end,
            self.styles.bits() as usize,
            self.non_printable_before,
            self.non_printable_after,
        ]
        .into_iter()
        .enumerate()
        {
            put(slot, value);
        }
        for (index, value) in [
            self.first_bidi,
            self.last_bidi,
            self.first_text_terminator,
            self.last_text_terminator,
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(value) = value {
                put(11 + index * 2, value.utf16 + 1); // Zero means absent, distinct from a terminator at display offset zero.
                put(12 + index * 2, value.byte_end);
            }
        }
        record
    }

    /// The original Reader rejects short reads. Invalid coordinate conversion or
    /// style bits remain storage failures instead of becoming default content.
    fn decode(record: [u8; RECORD_BYTES]) -> Result<Self, MarkdownDocumentBufferError> {
        let get = |slot: usize| -> Result<usize, MarkdownDocumentBufferError> {
            usize::try_from(u64::from_le_bytes(
                record[slot * 8..slot * 8 + 8]
                    .try_into()
                    .expect("固定 record 字段始终为八字节"),
            ))
            .map_err(|_| MarkdownDocumentBufferError::DisplayStorageUnavailable(None))
        };
        let terminator = |index: usize| -> Result<
            Option<DisplayProjectionTerminator>,
            MarkdownDocumentBufferError,
        > {
            let value = get(11 + index * 2)?;
            Ok(if value == 0 {
                None
            } else {
                Some(DisplayProjectionTerminator {
                    utf16: value - 1,
                    byte_end: get(12 + index * 2)?,
                })
            })
        };
        Ok(Self {
            display_bytes: get(0)?..get(1)?,
            display_utf16: get(2)?..get(3)?,
            raw_bytes: get(4)?..get(5)?,
            raw_utf16: get(6)?..get(7)?,
            styles: u32::try_from(get(8)?)
                .ok()
                .and_then(MarkdownPresentationStyles::from_bits)
                .ok_or(MarkdownDocumentBufferError::DisplayStorageUnavailable(None))?,
            non_printable_before: get(9)?,
            non_printable_after: get(10)?,
            first_bidi: terminator(0)?,
            last_bidi: terminator(1)?,
            first_text_terminator: terminator(2)?,
            last_text_terminator: terminator(3)?,
        })
    }
}

/// Retain one coalescing run and the index's original 16 KiB append buffer. Index
/// length does not become a resident Vec of runs or a second tree of text nodes.
pub(crate) struct DisplayProjectionBuilder {
    raw: MarkdownTextSnapshot,
    index: PagedLayoutIndex<RECORD_BYTES>,
    pending: Option<DisplayProjectionRun>,
    metrics: MarkdownDocumentMetrics,
    non_printable: usize,
}

impl DisplayProjectionBuilder {
    /// Observe this actual unsealed writer without keeping its raw snapshot, FD,
    /// or append buffer alive.
    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        self.index.resource_observer()
    }

    /// Report metadata actually appended for the caller's logical-work accounting.
    /// This is not a measure of resident memory or peak process usage.
    pub(crate) fn record_bytes(&self) -> usize {
        self.index.len() * RECORD_BYTES
    }
    /// The original cell factory has already sealed raw. The builder receives no
    /// active document, unverified range, or second copy of the text.
    pub(crate) fn new(raw: MarkdownTextSnapshot) -> Result<Self, MarkdownDocumentBufferError> {
        raw.original_backing_health()?;
        Ok(Self {
            raw,
            index: PagedLayoutIndex::new().map_err(index_error)?,
            pending: None,
            metrics: MarkdownDocumentMetrics::default(),
            non_printable: 0,
        })
    }

    /// Each converted scalar updates display metrics and one source run. Neither
    /// an appended text file nor a materialized whole-cell string is created.
    pub(crate) fn append(
        &mut self,
        scalar: MarkdownTableCellDisplayScalar,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.raw.original_backing_health()?;
        let before = self.non_printable;
        self.non_printable += usize::from(!(' '..='~').contains(&scalar.scalar));
        let terminal = DisplayProjectionTerminator {
            utf16: scalar.display_utf16.start,
            byte_end: scalar.display_bytes.end,
        };
        let bidi = (UnicodeBidiData.bidi_class(scalar.scalar) == BidiClass::B).then_some(terminal);
        let checking = matches!(scalar.scalar, '\r' | '\n' | '\u{2029}').then_some(terminal);
        if let Some(run) = self.pending.as_mut()
            && run.display_bytes.end == scalar.display_bytes.start
            && run.display_utf16.end == scalar.display_utf16.start
            && run.raw_bytes.end == scalar.raw_bytes.start
            && run.raw_utf16.end == scalar.raw_utf16.start
            && run.styles == scalar.styles
        {
            // Coalesce only when both raw and display coordinates continue and
            // style is unchanged. A removed marker must leave a real raw gap.
            run.display_bytes.end = scalar.display_bytes.end;
            run.display_utf16.end = scalar.display_utf16.end;
            run.raw_bytes.end = scalar.raw_bytes.end;
            run.raw_utf16.end = scalar.raw_utf16.end;
            run.non_printable_after = self.non_printable;
            run.first_bidi = run.first_bidi.or(bidi);
            run.last_bidi = bidi.or(run.last_bidi);
            run.first_text_terminator = run.first_text_terminator.or(checking);
            run.last_text_terminator = checking.or(run.last_text_terminator);
        } else {
            // Seal the previous run before retaining its successor. This keeps
            // only one pending record regardless of the cell's source length.
            self.flush()?;
            self.pending = Some(DisplayProjectionRun {
                display_bytes: scalar.display_bytes,
                display_utf16: scalar.display_utf16,
                raw_bytes: scalar.raw_bytes,
                raw_utf16: scalar.raw_utf16,
                styles: scalar.styles,
                non_printable_before: before,
                non_printable_after: self.non_printable,
                first_bidi: bidi,
                last_bidi: bidi,
                first_text_terminator: checking,
                last_text_terminator: checking,
            });
        }
        let mut encoded = [0; 4];
        self.metrics = MarkdownDocumentMetrics::joined(
            self.metrics,
            MarkdownDocumentMetrics::from_text(scalar.scalar.encode_utf8(&mut encoded)),
        );
        self.raw.original_backing_health()?;
        Ok(())
    }

    /// Append a fixed record only at a source/style break. Previously appended
    /// bytes in this inode are never rewritten during projection construction.
    fn flush(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if let Some(run) = self.pending.take() {
            self.index.append(run.encode()).map_err(index_error)?;
        }
        Ok(())
    }

    /// Seal only after the complete original input and semantics are known. Empty
    /// display output closes the writer rather than creating a nonexistent reader.
    pub(crate) fn finish(mut self) -> Result<Arc<DisplayProjection>, MarkdownDocumentBufferError> {
        self.raw.original_backing_health()?;
        self.flush()?;
        let count = self.index.len();
        let observer = self.index.resource_observer();
        let reader = if count == 0 {
            self.index.close().map_err(index_error)?;
            None
        } else {
            Some(self.index.into_reader().map_err(index_error)?)
        };
        self.raw.original_backing_health()?;
        Ok(Arc::new(DisplayProjection {
            raw: self.raw,
            reader: Mutex::new(reader),
            metrics: self.metrics,
            count,
            observer,
            first_failure: AtomicI32::new(0),
        }))
    }
}

/// The frozen raw pieces and this sealed index are the only strong resources.
/// Display text is borrowed through bounded visitor callbacks, never retained here.
pub(crate) struct DisplayProjection {
    raw: MarkdownTextSnapshot,
    reader: Mutex<Option<LayoutIndexReader<RECORD_BYTES>>>,
    metrics: MarkdownDocumentMetrics,
    count: usize,
    observer: LayoutIndexObserver,
    first_failure: AtomicI32,
}

/// Borrow the same sealed index and retain one pending record. The display range
/// is already clamped to this call's UTF-16 allowance, so coalescing styles cannot
/// scan all runs in a giant cell or create another FD, text buffer, or index.
pub(crate) struct DisplayProjectionVisibleRawRanges<'a> {
    projection: &'a DisplayProjection,
    display: Range<usize>,
    next: usize,
    pending: Option<DisplayProjectionRun>,
    limited: bool,
    done: bool,
}

impl DisplayProjectionVisibleRawRanges<'_> {
    pub(crate) fn source_limited(&self) -> bool {
        self.limited
    }
}

impl Iterator for DisplayProjectionVisibleRawRanges<'_> {
    type Item = Result<Range<usize>, MarkdownDocumentBufferError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = (|| {
            self.projection.health()?;
            let Some(mut run) = self
                .pending
                .take()
                .map(Ok)
                .or_else(|| {
                    (self.next < self.projection.count).then(|| {
                        let record = self.projection.run(self.next);
                        self.next += 1;
                        record
                    })
                })
                .transpose()?
            else {
                self.done = true;
                return Ok(None);
            };
            let start = self.display.start.max(run.display_utf16.start);
            let end = self.display.end.min(run.display_utf16.end);
            if start >= end {
                self.done = true;
                return Ok(None);
            }
            let mut raw = run.raw_utf16.start + start - run.display_utf16.start
                ..run.raw_utf16.start + end - run.display_utf16.start;
            // A style break does not stop a literal query across fonts; an actual
            // raw gap ends this visible contiguous span. Every record contains at
            // least one UTF-16 unit, bounding this loop by the admitted input range.
            while run.display_utf16.end < self.display.end && self.next < self.projection.count {
                let next = self.projection.run(self.next)?;
                self.next += 1;
                if next.raw_utf16.start != raw.end {
                    self.pending = Some(next);
                    break;
                }
                raw.end = next.raw_utf16.start + self.display.end.min(next.display_utf16.end)
                    - next.display_utf16.start;
                run = next;
            }
            self.done = run.display_utf16.end >= self.display.end;
            self.projection.health()?;
            Ok(Some(raw))
        })();
        match result {
            Ok(value) => value.map(Ok),
            Err(cause) => {
                self.done = true; // Emit the first error once; a later intact record cannot restore partial success.
                Some(Err(cause))
            }
        }
    }
}

impl DisplayProjection {
    /// Validate both requested display scalar boundaries before clamping the end
    /// to the caller's remaining UTF-16 allowance. Locate the first run once, then
    /// advance this borrowing cursor; an omitted raw gap is never filled by an envelope.
    pub(crate) fn visible_raw_ranges(
        &self,
        display: Range<usize>,
        maximum_units: usize,
    ) -> Result<DisplayProjectionVisibleRawRanges<'_>, MarkdownDocumentBufferError> {
        self.health()?;
        if display.start > display.end || display.end > self.metrics.utf16_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        self.byte_offset_for_utf16(display.start)?;
        self.byte_offset_for_utf16(display.end)?;
        let mut end = display.start.saturating_add(maximum_units).min(display.end);
        match self.byte_offset_for_utf16(end) {
            Ok(_) => {}
            Err(MarkdownDocumentBufferError::NotUnicodeBoundary) if end > display.start => {
                end -= 1; // Only a split surrogate pair needs one-unit retreat; actual Source errors remain errors.
                self.byte_offset_for_utf16(end)?;
            }
            Err(cause) => return Err(cause),
        }
        let next = if display.start == end {
            self.count
        } else {
            self.locate(display.start, 1)?
        };
        Ok(DisplayProjectionVisibleRawRanges {
            projection: self,
            display: display.start..end,
            next,
            pending: None,
            limited: end < display.end,
            done: display.start == end,
        })
    }

    /// Raw algorithms read only this frozen root, retaining the original 4 KiB
    /// logical lease boundary for each actual borrow.
    pub(crate) fn raw(&self) -> &MarkdownTextSnapshot {
        &self.raw
    }
    /// Metrics were accumulated from visible scalars during conversion. Raw-tree
    /// aggregates cannot substitute for these independent display facts.
    pub(super) fn metrics(&self) -> MarkdownDocumentMetrics {
        self.metrics
    }
    /// The record count bounds binary search and per-record work without turning
    /// document length into a resident allocation.
    pub(crate) fn run_count(&self) -> usize {
        self.count
    }
    /// Report the actual sealed metadata extent, including the final pending run.
    pub(crate) fn record_bytes(&self) -> usize {
        self.count * RECORD_BYTES
    }
    /// The atomic observer keeps neither the index Reader nor raw mapping alive.
    pub(crate) fn resource_observer(&self) -> LayoutIndexObserver {
        self.observer.clone()
    }

    /// Original-byte failure takes precedence over a derived-index failure. Once
    /// this index is revoked, another intact record cannot make it healthy again.
    pub(crate) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        self.raw.original_backing_health()?;
        let failure = self.first_failure.load(Ordering::Acquire);
        if failure != 0 {
            return Err(MarkdownDocumentBufferError::DisplayStorageUnavailable(
                if failure == UNKNOWN_FAILURE {
                    None
                } else {
                    Some(failure)
                },
            ));
        }
        let state = self.observer.snapshot();
        if state.cleanup_failed || state.reader_failed {
            return Err(self.fail(MarkdownDocumentBufferError::DisplayStorageUnavailable(None)));
        }
        Ok(())
    }

    /// Retain the first actual derived-storage failure without promoting ordinary
    /// range errors to I/O or hiding the original raw Source's errno.
    fn fail(&self, error: MarkdownDocumentBufferError) -> MarkdownDocumentBufferError {
        if let MarkdownDocumentBufferError::DisplayStorageUnavailable(errno) = error {
            let _ = self.first_failure.compare_exchange(
                0,
                errno.unwrap_or(UNKNOWN_FAILURE),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        self.raw.original_backing_health().err().unwrap_or(error)
    }

    /// Read one original 256-byte record at a time, without prefetch or another
    /// Source, text cache, or in-memory run index.
    pub(crate) fn run(
        &self,
        ordinal: usize,
    ) -> Result<DisplayProjectionRun, MarkdownDocumentBufferError> {
        self.health()?;
        if ordinal >= self.count {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        let result = (|| {
            // Serialize only this reader's fixed-record I/O. Raw-source health is
            // checked after the lock has left scope, before interpreting success.
            let mut reader = self
                .reader
                .lock()
                .map_err(|_| MarkdownDocumentBufferError::DisplayStorageUnavailable(None))?;
            let record = reader
                .as_mut()
                .expect("非空封存显示目录必须拥有原 Reader")
                .read(ordinal)
                .map_err(index_error)?;
            DisplayProjectionRun::decode(record)
        })();
        self.raw.original_backing_health()?;
        result.map_err(|error| self.fail(error))
    }

    /// Find the first run whose end is strictly after the position. Actual EOF is
    /// count; a neighboring cell never supplies a missing coordinate.
    fn locate(&self, position: usize, axis: u8) -> Result<usize, MarkdownDocumentBufferError> {
        let mut low = 0;
        let mut high = self.count;
        while low != high {
            let middle = low + (high - low) / 2;
            let run = self.run(middle)?;
            let end = match axis {
                0 => run.display_bytes.end,
                1 => run.display_utf16.end,
                _ => run.raw_utf16.end,
            };
            if end <= position {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }

    /// Display UTF-16 must identify an actual scalar boundary. Linear translation
    /// inside a raw run cannot turn a surrogate interior into a valid caret.
    pub(crate) fn byte_offset_for_utf16(
        &self,
        position: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.health()?;
        if position > self.metrics.utf16_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        // The sealed display origin is always display byte zero, even when raw
        // syntax before it was omitted. Health was checked first; repeated native
        // boundary observations need not reread the first disk record for zero.
        if position == 0 {
            return Ok(0);
        }
        if position == self.metrics.utf16_len {
            return Ok(self.metrics.byte_len);
        }
        let run = self.run(self.locate(position, 1)?)?;
        let raw = self
            .raw
            .byte_offset_for_utf16(run.raw_utf16.start + position - run.display_utf16.start)?;
        Ok(run.display_bytes.start + raw - run.raw_bytes.start)
    }

    /// The same raw snapshot validates byte/scalar boundaries. Hidden gaps change
    /// coordinate offsets but do not permit dropping part of a visible scalar.
    pub(crate) fn utf16_offset_for_byte(
        &self,
        position: usize,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.health()?;
        if position > self.metrics.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if position == self.metrics.byte_len {
            return Ok(self.metrics.utf16_len);
        }
        let run = self.run(self.locate(position, 0)?)?;
        let raw = self
            .raw
            .utf16_offset_for_byte(run.raw_bytes.start + position - run.display_bytes.start)?;
        Ok(run.display_utf16.start + raw - run.raw_utf16.start)
    }

    /// Borrow each display fragment directly from original pieces. A shaping
    /// caller may concatenate them into its own buffer of at most 4 KiB as needed.
    pub(crate) fn visit_byte_range(
        &self,
        range: Range<usize>,
        visit: &mut dyn FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        if range.start > range.end || range.end > self.metrics.byte_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.is_empty() {
            return Ok(());
        }
        let mut ordinal = self.locate(range.start, 0)?;
        let mut position = range.start;
        while position != range.end {
            let run = self.run(ordinal)?;
            let end = range.end.min(run.display_bytes.end);
            let start = run.raw_bytes.start + position - run.display_bytes.start;
            let result = self
                .raw
                // Dynamic dispatch closes the raw/display forwarding type here;
                // recursive forwarding must not instantiate ever deeper &mut types.
                .visit_byte_range(start..start + end - position, &mut *visit);
            self.health()?;
            result?;
            position = end;
            ordinal += 1;
        }
        Ok(())
    }

    /// Preserve NSString's code-unit reads, including surrogate halves. Traverse
    /// actual runs without constructing whole UTF-16 text or another cache.
    pub(crate) fn copy_utf16_code_units(
        &self,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.health()?;
        let end = offset
            .checked_add(destination.len())
            .filter(|end| *end <= self.metrics.utf16_len)
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        if destination.is_empty() {
            return Ok(());
        }
        let mut ordinal = self.locate(offset, 1)?;
        let mut position = offset;
        while position != end {
            let run = self.run(ordinal)?;
            let count = (end - position).min(run.display_utf16.end - position);
            let start = run.raw_utf16.start + position - run.display_utf16.start;
            self.raw.copy_utf16_code_units(
                start,
                &mut destination[position - offset..position - offset + count],
            )?;
            position += count;
            ordinal += 1;
        }
        self.health()
    }

    /// Prefix counts cover complete middle runs. Decode bounded raw leaves only
    /// for the two partial ends, validating both scalar endpoints even on a miss.
    pub(crate) fn is_printable_ascii_range(
        &self,
        range: Range<usize>,
    ) -> Result<bool, MarkdownDocumentBufferError> {
        self.byte_offset_for_utf16(range.start)?;
        self.byte_offset_for_utf16(range.end)?;
        if range.start > range.end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if range.is_empty() || self.metrics.all_printable_ascii.is_printable() {
            return Ok(true);
        }
        let first_index = self.locate(range.start, 1)?;
        let last_index = self.locate(range.end - 1, 1)?;
        let first = self.run(first_index)?;
        let last = self.run(last_index)?;
        let first_end = range.end.min(first.display_utf16.end);
        let first_raw = first.raw_utf16.start + range.start - first.display_utf16.start;
        let beginning = self
            .raw
            .is_printable_ascii_range(first_raw..first_raw + first_end - range.start)?;
        if first_index == last_index {
            return Ok(beginning);
        }
        let last_end = last.raw_utf16.start + range.end - last.display_utf16.start;
        let ending = self
            .raw
            .is_printable_ascii_range(last.raw_utf16.start..last_end)?;
        self.health()?;
        Ok(beginning && ending && first.non_printable_after == last.non_printable_before)
    }

    /// Text checking accepts surrogate-interior code-unit positions. Locate actual
    /// visible terminators so a hidden raw separator cannot split a display paragraph.
    pub(crate) fn text_checking_paragraph_range(
        &self,
        request: Range<usize>,
    ) -> Result<Range<usize>, MarkdownDocumentBufferError> {
        self.health()?;
        if request.start > request.end || request.end > self.metrics.utf16_len {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if self.metrics.text_checking_terminators == 0 {
            return Ok(0..self.metrics.utf16_len);
        }
        let start = self
            .previous_text_terminator(request.start)?
            .map_or(0, |position| position + 1);
        let anchor = if request.is_empty() {
            request.end
        } else {
            request.end - 1
        };
        let Some(terminal) = self.next_text_terminator(anchor)? else {
            return Ok(start..self.metrics.utf16_len);
        };
        let mut end = terminal + 1;
        let mut unit = [0];
        self.copy_utf16_code_units(terminal, &mut unit)?;
        if unit[0] == 13 && end < self.metrics.utf16_len {
            self.copy_utf16_code_units(end, &mut unit)?;
            if unit[0] == 10 {
                end += 1;
            } // Merge the actual display CRLF; raw adjacency cannot bridge an omitted span.
        }
        self.health()?;
        Ok(start..end)
    }

    /// Complete middle runs use their actual last-terminator summary. Only the
    /// endpoint run needs the original equal-length raw range searched internally.
    fn previous_text_terminator(
        &self,
        before: usize,
    ) -> Result<Option<usize>, MarkdownDocumentBufferError> {
        if before == 0 || self.count == 0 {
            return Ok(None);
        }
        let mut ordinal = self.locate(before - 1, 1)?;
        loop {
            let run = self.run(ordinal)?;
            if let Some(last) = run.last_text_terminator {
                if last.utf16 < before {
                    return Ok(Some(last.utf16));
                }
                if run
                    .first_text_terminator
                    .is_some_and(|first| first.utf16 < before)
                {
                    let raw_at = run.raw_utf16.start + before - run.display_utf16.start;
                    let range = self.raw.text_checking_paragraph_range(raw_at..raw_at)?;
                    if range.start > run.raw_utf16.start {
                        return Ok(Some(
                            run.display_utf16.start + range.start - 1 - run.raw_utf16.start,
                        ));
                    }
                }
            }
            let Some(previous) = ordinal.checked_sub(1) else {
                return Ok(None);
            };
            ordinal = previous;
        }
    }

    /// Raw paragraph lookup may merge CRLF. Accept only a terminator inside this
    /// run; a result beyond its visible extent requires searching the next run.
    fn next_text_terminator(
        &self,
        at: usize,
    ) -> Result<Option<usize>, MarkdownDocumentBufferError> {
        if at == self.metrics.utf16_len || self.count == 0 {
            return Ok(None);
        }
        let mut ordinal = self.locate(at, 1)?;
        while ordinal < self.count {
            let run = self.run(ordinal)?;
            if let Some(first) = run.first_text_terminator {
                if first.utf16 >= at {
                    return Ok(Some(first.utf16));
                }
                if run
                    .last_text_terminator
                    .is_some_and(|last| last.utf16 >= at)
                {
                    let raw_at = run.raw_utf16.start + at - run.display_utf16.start;
                    let range = self.raw.text_checking_paragraph_range(raw_at..raw_at)?;
                    let mut terminal = range.end - 1;
                    if terminal > raw_at {
                        let mut pair = [0; 2];
                        self.raw.copy_utf16_code_units(terminal - 1, &mut pair)?;
                        if pair == [13, 10] {
                            terminal -= 1;
                        }
                    }
                    if run.raw_utf16.contains(&terminal) {
                        return Ok(Some(
                            run.display_utf16.start + terminal - run.raw_utf16.start,
                        ));
                    }
                }
            }
            ordinal += 1;
        }
        Ok(None)
    }

    /// Affinity chooses the side of hidden syntax at a display boundary. A range
    /// end uses the upstream visible tail so a following URL/closer is not consumed.
    pub(crate) fn display_to_raw(
        &self,
        position: usize,
        downstream: bool,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.byte_offset_for_utf16(position)?;
        if self.count == 0 {
            return Ok(0);
        }
        if position == self.metrics.utf16_len {
            return Ok(self.run(self.count - 1)?.raw_utf16.end);
        }
        // Validate position itself first. position - 1 selects a directory run;
        // it is not exposed as a scalar boundary or used for a byte borrow.
        let lookup = if !downstream && position != 0 {
            position - 1
        } else {
            position
        };
        let run = self.run(self.locate(lookup, 1)?)?;
        Ok(run.raw_utf16.start + position - run.display_utf16.start)
    }

    /// The actual caller chooses affinity inside a hidden raw gap. A directed
    /// selection is not guessed to be the trailing edge of a display range.
    pub(crate) fn raw_to_display(
        &self,
        position: usize,
        downstream: bool,
    ) -> Result<usize, MarkdownDocumentBufferError> {
        self.health()?;
        self.raw.byte_offset_for_utf16(position)?;
        if self.count == 0 {
            return Ok(0);
        }
        let ordinal = self.locate(position, 2)?;
        if ordinal == self.count {
            return Ok(self.metrics.utf16_len);
        }
        let run = self.run(ordinal)?;
        if position >= run.raw_utf16.start {
            return Ok(run.display_utf16.start + position - run.raw_utf16.start);
        }
        if downstream || ordinal == 0 {
            Ok(run.display_utf16.start)
        } else {
            Ok(self.run(ordinal - 1)?.display_utf16.end)
        }
    }

    /// Return styles only from this sealed index's actual output run. EOF and an
    /// empty cell have no character whose style could be queried.
    pub(crate) fn styles_at(
        &self,
        position: usize,
    ) -> Result<Option<crate::MarkdownPresentationQuery>, MarkdownDocumentBufferError> {
        self.byte_offset_for_utf16(position)?;
        if position == self.metrics.utf16_len {
            return Ok(None);
        }
        let run = self.run(self.locate(position, 1)?)?;
        Ok(Some(crate::MarkdownPresentationQuery {
            utf16_range: run.display_utf16,
            styles: run.styles,
        }))
    }
}

/// Preserve errno for actual index I/O. Poisoned locks or revoked indices carry
/// None rather than inventing a system-level cause.
fn index_error(error: LayoutIndexError) -> MarkdownDocumentBufferError {
    MarkdownDocumentBufferError::DisplayStorageUnavailable(match error {
        LayoutIndexError::Io(error) => error.raw_os_error(),
        _ => None,
    })
}
