//! Completed runs and bracket frames use separate ordered/addressed indexes.
//! Each owns one real 16-KiB write buffer, for a fixed combined 32-KiB ceiling.
use super::{
    MarkdownScript as Script, MarkdownScriptCancellation as Cancellation,
    MarkdownScriptError as Error, MarkdownScriptRun as Run, Result,
};
use crate::markdown_layout_index::{LayoutIndexError, LayoutIndexObserver, PagedLayoutIndex};

pub(super) const NONE: usize = usize::MAX;
pub(super) struct Frame {
    pub(super) previous: usize,
    pub(super) same_kind: usize,
    pub(super) run: usize,
    pub(super) kind: u8,
}
pub(super) struct Store {
    runs: PagedLayoutIndex<64>,
    frames: PagedLayoutIndex<32>,
    end_byte: usize,
    end_utf16: usize,
    cancellation: Cancellation,
}
impl From<LayoutIndexError> for Error {
    fn from(error: LayoutIndexError) -> Self {
        match error {
            LayoutIndexError::InvalidRecord => Self::CorruptMetadata,
            LayoutIndexError::Overflow => Self::CoordinateOverflow,
            LayoutIndexError::Poisoned => Self::Poisoned,
            LayoutIndexError::Io(error) => Self::Storage(error),
        }
    }
}
/// Encodes checked machine coordinates in the fixed on-disk record format,
/// without truncating an addressable value to fit a smaller integer.
fn put(record: &mut [u8], field: usize, value: usize) -> Result<()> {
    record[field..field + 8].copy_from_slice(
        &u64::try_from(value)
            .map_err(|_| Error::CoordinateOverflow)?
            .to_le_bytes(),
    );
    Ok(())
}
/// Decodes the same fixed-width field and refuses values this process cannot
/// address instead of treating wrapped offsets as valid source coordinates.
fn get(record: &[u8], field: usize) -> Result<usize> {
    let bytes: [u8; 8] = record[field..field + 8].try_into().expect("固定八字节字段");
    usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| Error::CoordinateOverflow)
}
impl Store {
    /// Table bodies have no SCX runs. This coverage cursor accepts original physical
    /// hard boundaries without filling skipped source text with Common runs.
    #[cfg(all(feature = "s2-table-window-prototype", feature = "paged-line-compose"))]
    pub(super) fn skip_table(
        &mut self,
        span: crate::markdown_layout_source::table_rows::TableSpan,
    ) -> Result<()> {
        self.cancellation.check()?;
        if self.end_byte < span.start.byte || self.end_byte > span.end.byte {
            return Err(Error::CorruptMetadata);
        }
        self.end_byte = span.end.byte;
        self.end_utf16 = span.end.utf16;
        Ok(())
    }
    /// The same fold proof moves coverage only. Hidden text has no script value;
    /// Unknown or empty runs must not be manufactured to fill that hole.
    #[cfg(feature = "paged-line-compose")]
    pub(super) fn skip_fold(
        &mut self,
        span: &crate::markdown_layout_source::folds::FoldSpan,
    ) -> Result<()> {
        self.cancellation.check()?;
        if self.end_byte != span.header_end.byte || self.end_utf16 != span.header_end.utf16 {
            return Err(Error::CorruptMetadata);
        }
        self.end_byte = span.end.byte;
        self.end_utf16 = span.end.utf16;
        Ok(())
    }

    /// Acquires separate anonymous run/frame indexes under one cancellation
    /// authority. Partial construction releases already acquired local owners.
    pub(super) fn new(cancellation: &Cancellation) -> Result<Self> {
        cancellation.check()?;
        let runs = PagedLayoutIndex::new()?;
        cancellation.check()?; // Cancellation during first-index acquisition drops that actual local owner.
        let frames = PagedLayoutIndex::new()?;
        cancellation.check()?;
        Ok(Self {
            runs,
            frames,
            end_byte: 0,
            end_utf16: 0,
            cancellation: cancellation.clone(),
        })
    }
    /// Uses a real caller-owned descriptor for storage-permission regression;
    /// production encoding, buffering, page I/O, and cancellation remain unchanged.
    #[cfg(test)]
    pub(super) fn with_runs_file(file: std::fs::File, cancellation: &Cancellation) -> Result<Self> {
        Ok(Self {
            runs: PagedLayoutIndex::test_with_file(file)?,
            frames: PagedLayoutIndex::new()?,
            end_byte: 0,
            end_utf16: 0,
            cancellation: cancellation.clone(),
        }) // Only actual descriptor permissions differ; original buffered/page writes still compute results.
    }
    /// Returns committed record count without reading source text or file pages.
    pub(super) fn run_count(&self) -> usize {
        self.runs.len()
    }
    /// Borrows initialized record bounds from the sole append page, without copying
    /// it, reading older file pages, or creating a successor cache.
    /// Callers still decode selected real records with `run`. Range inspection
    /// cannot bypass cancellation or the index's known first failure.
    pub(super) fn run_append_page(&mut self) -> Result<Option<std::ops::Range<usize>>> {
        self.cancellation.check()?;
        self.runs.append_page_records().map_err(Into::into)
    }
    /// Returns actual allocated frame IDs, not the current bracket-stack depth.
    pub(super) fn frame_count(&self) -> usize {
        self.frames.len()
    }
    /// Publishes one nonempty contiguous run. Coverage endpoints advance only
    /// after its real fixed record enters the owned append index successfully.
    pub(super) fn append_run(&mut self, run: Run) -> Result<()> {
        self.cancellation.check()?; // Check this same cancellation handle on every metadata operation, including deep stacks.
        if run.start_byte != self.end_byte
            || run.start_utf16 != self.end_utf16
            || run.start_byte >= run.end_byte
            || run.start_utf16 >= run.end_utf16
        {
            return Err(Error::CorruptMetadata); // Publish actual contiguous nonempty coverage; empty runs cannot fill unparsed prefixes.
        }
        let mut record = [0; 64];
        for (field, value) in [
            (0, run.start_byte),
            (8, run.end_byte),
            (16, run.start_utf16),
            (24, run.end_utf16),
        ] {
            put(&mut record, field, value)?;
        }
        record[32] = run.script.0;
        self.runs.append(record)?;
        self.end_byte = run.end_byte;
        self.end_utf16 = run.end_utf16;
        Ok(())
    }
    /// Reads and validates one actual run record; malformed coordinates or a
    /// script outside the pinned table revoke this metadata result.
    pub(super) fn run(&mut self, index: usize) -> Result<Run> {
        self.cancellation.check()?;
        let record = self.runs.read(index)?;
        let run = Run {
            start_byte: get(&record, 0)?,
            end_byte: get(&record, 8)?,
            start_utf16: get(&record, 16)?,
            end_utf16: get(&record, 24)?,
            script: Script(record[32]),
        };
        if run.start_byte >= run.end_byte
            || run.start_utf16 >= run.end_utf16
            || usize::from(run.script.0) >= super::unicode_data::TAGS.len()
        {
            return Err(Error::CorruptMetadata);
        }
        Ok(run)
    }
    /// Appends one bracket frame with backward-only links. A frame may refer
    /// to the currently open run ID, but never a future run or forward frame.
    pub(super) fn append_frame(&mut self, frame: Frame) -> Result<usize> {
        self.cancellation.check()?;
        let index = self.frames.len();
        if frame.kind >= 65
            || frame.run > self.runs.len()
            || [frame.previous, frame.same_kind]
                .into_iter()
                .any(|value| value != NONE && value >= index)
        {
            return Err(Error::CorruptMetadata);
        }
        let mut record = [0; 32];
        for (field, value) in [(0, frame.previous), (8, frame.same_kind), (16, frame.run)] {
            put(&mut record, field, value)?;
        }
        record[24] = frame.kind;
        self.frames.append(record)?;
        Ok(index)
    }
    /// Revalidates persisted pairing links before they can change stack heads
    /// or constrain a closing bracket's script.
    pub(super) fn frame(&mut self, index: usize) -> Result<Frame> {
        self.cancellation.check()?;
        let record = self.frames.read(index)?;
        let frame = Frame {
            previous: get(&record, 0)?,
            same_kind: get(&record, 8)?,
            run: get(&record, 16)?,
            kind: record[24],
        };
        if frame.kind >= 65
            || frame.run > self.runs.len()
            || [frame.previous, frame.same_kind]
                .into_iter()
                .any(|value| value != NONE && value >= index)
        {
            return Err(Error::CorruptMetadata);
        }
        Ok(frame)
    }
    /// Once actual pairing work ends, close its no-longer-needed index rather
    /// than retaining an idle descriptor/write buffer throughout layout.
    pub(super) fn seal_frames(&mut self) -> Result<()> {
        self.cancellation.check()?;
        self.frames.close().map_err(Into::into)
    }
    /// Seals completed run storage for later bounded reads; retained run data
    /// and physically returned append resources remain distinct facts.
    pub(super) fn seal_runs(&mut self) -> Result<()> {
        self.cancellation.check()?;
        self.runs.seal().map_err(Into::into)
    }
    /// Attempts both owned closes before propagating failure. Cancellation must
    /// not prevent cleanup of resources this operation already acquired.
    pub(super) fn close(&mut self) -> Result<()> {
        let frames = self.frames.close();
        let runs = self.runs.close();
        frames?;
        runs?;
        Ok(()) // Attempt both actual closes before returning the first failure; never leave another mapping by short-circuiting.
    }
    /// Revokes both derived indexes together; later metadata hits cannot revive
    /// a partially failed source/layout construction.
    pub(super) fn poison(&mut self) {
        self.frames.poison();
        self.runs.poison();
    }
    /// Returns the two original fixed-fact observers without retaining either
    /// file, mapping, or document text as another diagnostic owner.
    pub(super) fn observers(&self) -> (LayoutIndexObserver, LayoutIndexObserver) {
        (
            self.runs.resource_observer(),
            self.frames.resource_observer(),
        )
    }
    /// Reports actual separate index-page loads for profiling, never inferred
    /// source reads or a kernel-RSS reclamation receipt.
    pub(super) fn loads(&self) -> (u64, u64) {
        (self.runs.page_loads(), self.frames.page_loads())
    }
}
