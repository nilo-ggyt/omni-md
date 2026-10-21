//! While the original worker appends, publish only a same-inode read-only prefix
//! ending at an actual complete LineEnd.

use super::{output::OutputSeal, *};
use crate::markdown_layout_index::LayoutIndexSnapshotError;

impl From<LayoutIndexSnapshotError> for Error {
    fn from(error: LayoutIndexSnapshotError) -> Self {
        match error {
            LayoutIndexSnapshotError::ReadersBusy => Self::SnapshotBusy,
            LayoutIndexSnapshotError::Index(error) => Self::from(error),
        } // Reader-slot backpressure belongs to this entry only; preserve other
        // index consumers' original typed I/O classifications.
    }
}

impl GlyphStore {
    /// Deliver no candidate without a complete line or while capture is partial.
    /// Existing snapshots do not prevent subsequent same-Source appends.
    pub(in crate::markdown_line_composer) fn publish_ready_prefix(
        &mut self,
    ) -> Result<Option<MarkdownComposedLines>> {
        let result = (|| {
            self.guard()?;
            if self.capture.is_some() || self.control_capture.is_some() || self.lines == 0 {
                return Ok(None);
            } // Keep the worker's original stage; a partial item is not a committed line.
            let last = self.line_end.ok_or(Error::InconsistentCoverage)?;
            let records = last.checked_add(1).ok_or(Error::Overflow)?;
            let graphemes = self.published_grapheme_directory()?;
            let reader = match self.index.as_mut().ok_or(Error::Poisoned)? {
                Rows::Writing(index) => index.snapshot_reader(records)?,
                #[cfg(test)]
                Rows::Reading(_) => return Err(Error::InvalidRequest),
            };
            // Recheck original Source/writer after actual checkpoint/dup. An invalid
            // candidate's owned descriptor is released with that local reader.
            self.guard()?;
            Ok(Some(MarkdownComposedLines::from_sealed(OutputSeal {
                source: self.source.clone(),
                authority: self.authority.clone(),
                fonts: self.fonts.clone(),
                cancellation: self.cancellation.clone(),
                reader,
                // Each delivery gets its own output identity. Equal inode, line
                // ordinal, or record cannot renew an old Part across snapshots.
                identity: Arc::new(()),
                #[cfg(feature = "paged-shaping-frontier")]
                lineage: Arc::downgrade(&self.identity),
                last: Some(last),
                count: self.lines,
                range: 0..self.line_source_end,
                graphemes,
                folds: self.folds.clone(),
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                tables: self.tables.clone(),
            })?))
        })();
        if matches!(result, Err(Error::SnapshotBusy)) {
            // Two retained real readers impose publication backpressure. Health
            // failure still wins; this result cannot bypass Source/I/O revocation.
            if let Err(error) = self.guard() {
                return self.finish(Err(error));
            }
            return result;
        }
        self.finish(result)
    }
}
