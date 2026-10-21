//! Bounded heap and uniquely owned anonymous output for the same cell sanitization.
//! Long results retain no whole-cell heap copy.

use super::*;
use crate::immutable_snapshot::CanonicalSnapshotOutput;
use crate::markdown_viewport_presentation::table_window::TABLE_WINDOW_MAX_BYTES;

/// After admission, retain only final byte storage, not the raw root, document or
/// a third Scene-reader slot.
pub(super) enum CellReplacement {
    Inline(String),
    Frozen(Arc<OriginalBacking>),
}

impl CellReplacement {
    /// Canonical admission uses actual output byte length; mapped file extent is
    /// not heap allocation. Observe only accepted extent without copying text,
    /// including while the output still has an unflushed buffer.
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Inline(text) => text.len(),
            Self::Frozen(backing) => backing.len(),
        }
    }

    /// The unique commit claim records only actual heap-text capacity. The real
    /// backing owner remains responsible for sealed file resources.
    pub(super) fn capacity(&self) -> usize {
        match self {
            Self::Inline(text) => text.capacity(),
            Self::Frozen(_) => 0,
        }
    }

    /// The candidate file is not yet canonical Source; its failure cannot poison
    /// the unchanged raw draft's text authority.
    pub(super) fn health(&self) -> Result<(), MarkdownDocumentBufferError> {
        match self {
            Self::Inline(_) => Ok(()),
            Self::Frozen(backing) => backing
                .health()
                .map_err(|error| edit_read_error(error.into(), false)),
        }
    }

    /// Compare one actual finite fragment of canonical text. A mismatch must not
    /// make the caller skip later Source reads and their possible real failures.
    pub(super) fn matches_bytes(
        &self,
        offset: usize,
        bytes: &[u8],
    ) -> Result<bool, MarkdownDocumentBufferError> {
        match self {
            Self::Inline(text) => {
                Ok(text.as_bytes().get(offset..offset + bytes.len()) == Some(bytes))
            }
            Self::Frozen(backing) => {
                let lease = backing
                    .lease(offset..offset + bytes.len())
                    .map_err(|error| edit_read_error(error.into(), false))?;
                let same = &*lease == bytes;
                lease
                    .finish()
                    .map_err(|error| edit_read_error(error.into(), false))?;
                Ok(same)
            }
        }
    }

    /// Feed this same sealed result into the original PreparedTextInput. Introduce
    /// no second public commit receipt or repeated sanitization stage.
    pub(super) fn input(&self, utf16: usize) -> prepared_text_input::TextInputReplacement<'_> {
        match self {
            Self::Inline(text) => prepared_text_input::TextInputReplacement::Text(text),
            Self::Frozen(backing) => prepared_text_input::TextInputReplacement::Frozen {
                backing: Arc::clone(backing),
                utf16,
            },
        }
    }

    #[cfg(test)]
    /// Actual business tests observe this candidate's final munmap rather than
    /// infer file release from a returned numeric admission claim.
    pub(super) fn backing_observer(&self) -> Option<crate::MarkdownOriginalBackingObserver> {
        match self {
            Self::Inline(_) => None,
            Self::Frozen(backing) => Some(backing.observer()),
        }
    }
}

enum OutputStorage {
    Inline(String),
    File(CanonicalSnapshotOutput),
}

/// Sanitization and actual table-tail output share one writer and one 4 KiB
/// scratch. Seal the unique writer only when all output is complete.
pub(super) struct CellReplacementOutput {
    storage: OutputStorage,
    bytes: usize,
    utf16: usize,
    scratch: [u8; ORIGINAL_PIECE_BYTES],
    scratch_len: usize,
}

impl CellReplacementOutput {
    /// The 64 Ki-unit threshold selects bounded String output only. Larger input
    /// writes directly to an anonymous file instead of becoming a feature refusal.
    pub(super) fn new(
        metrics: MarkdownDocumentMetrics,
        prefix: &str,
    ) -> Result<Self, MarkdownDocumentBufferError> {
        let storage = if metrics.utf16_len <= MARKDOWN_CELL_DRAFT_COMMIT_UTF16_UNITS {
            let capacity = metrics
                .byte_len
                .saturating_add(metrics.utf16_len)
                .saturating_add(prefix.len())
                .min(TABLE_WINDOW_MAX_BYTES);
            OutputStorage::Inline(String::with_capacity(capacity))
        } else {
            OutputStorage::File(CanonicalSnapshotOutput::new().map_err(edit_snapshot_error)?)
        };
        let mut output = Self {
            storage,
            bytes: 0,
            utf16: 0,
            scratch: [0; ORIGINAL_PIECE_BYTES],
            scratch_len: 0,
        };
        output.push_text(prefix)?; // Write verified implicit separators first, outside the text's global trim state.
        Ok(output)
    }

    pub(super) fn len(&self) -> usize {
        self.bytes
    }
    /// UTF-16 length accumulated during these same scalar writes, used for the
    /// candidate's selection and original change journal.
    pub(super) fn utf16_len(&self) -> usize {
        self.utf16
    }

    /// Large trailing padding or container prefixes cannot expand a small cell
    /// into an unbounded String. Promote through the same anonymous factory first.
    fn require_file(&mut self, next_bytes: usize) -> Result<(), MarkdownDocumentBufferError> {
        if next_bytes <= TABLE_WINDOW_MAX_BYTES {
            return Ok(()); // The bounded heap path still fits; small commits need no extra file operation.
        }
        self.promote()
    }

    /// An oversized verified source tail must stream to disk; many small pushes
    /// cannot accumulate that tail into another large String.
    pub(super) fn prepare_suffix(
        &mut self,
        bytes: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        if bytes > EDIT_SEGMENT_BYTES {
            self.promote()?;
        }
        self.require_file(
            self.bytes
                .checked_add(bytes)
                .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?,
        )
    }

    /// Transfer the complete bounded heap output to one anonymous file before
    /// releasing its String. Failure discards the entire candidate.
    fn promote(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if let OutputStorage::Inline(text) = &self.storage {
            let mut file = CanonicalSnapshotOutput::new().map_err(edit_snapshot_error)?;
            let mut offset = 0;
            while offset < text.len() {
                let mut end = (offset + ORIGINAL_PIECE_BYTES).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                } // Transfer bounded heap bytes only between complete encoded scalars.
                file.write_all(&text.as_bytes()[offset..end])
                    .map_err(edit_snapshot_error)?;
                offset = end;
            }
            self.storage = OutputStorage::File(file); // Drop the String only after all its original bounded bytes were written successfully.
        }
        Ok(())
    }

    /// Accept a complete UTF-8 fragment and write through 4 KiB chunks. The owning
    /// transaction still admits the final document/session capacity. Any write
    /// failure requires discarding this candidate, not reusing partial output.
    pub(super) fn push_text(&mut self, text: &str) -> Result<(), MarkdownDocumentBufferError> {
        let next = self
            .bytes
            .checked_add(text.len())
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        let units = self
            .utf16
            .checked_add(text.encode_utf16().count())
            .ok_or(MarkdownDocumentBufferError::DocumentTooLarge)?;
        self.require_file(next)?;
        match &mut self.storage {
            OutputStorage::Inline(output) => output.push_str(text),
            OutputStorage::File(_) => {
                let mut offset = 0;
                while offset < text.len() {
                    if self.scratch_len == self.scratch.len() {
                        self.flush()?;
                    } // Actually write a full block first, without retaining a second output buffer.
                    let available = self.scratch.len() - self.scratch_len;
                    let mut end = (offset + available).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    if end == offset {
                        self.flush()?; // The next whole scalar does not fit the tail; resume with an empty scratch.
                        continue;
                    }
                    let chunk = &text.as_bytes()[offset..end];
                    self.scratch[self.scratch_len..self.scratch_len + chunk.len()]
                        .copy_from_slice(chunk);
                    self.scratch_len += chunk.len();
                    offset = end;
                }
            }
        }
        self.bytes = next;
        self.utf16 = units;
        Ok(())
    }

    /// A Source callback supplies only the current actual fragment. Its owner
    /// still returns the original lease before propagating UTF-8 failure.
    pub(super) fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), MarkdownDocumentBufferError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)?;
        self.push_text(text)
    }

    /// Global trim retracts only the already-written tail. Long trailing whitespace
    /// stays uncached, and subsequent table-tail output resumes at the new EOF.
    pub(super) fn truncate(
        &mut self,
        bytes: usize,
        utf16: usize,
    ) -> Result<(), MarkdownDocumentBufferError> {
        self.flush()?;
        let bytes = match &mut self.storage {
            OutputStorage::Inline(text) => {
                text.truncate(bytes);
                text.len()
            }
            OutputStorage::File(file) => {
                file.truncate(bytes).map_err(edit_snapshot_error)?;
                file.len()
            }
        };
        self.bytes = bytes;
        self.utf16 = utf16;
        Ok(())
    }

    /// Flush before seal and consume the unique File writer. The original backing
    /// owner performs actual descriptor closure for the resulting mapping.
    pub(super) fn seal(
        mut self,
        claim: &mut super::resources::CommitClaim,
    ) -> Result<(CellReplacement, usize), MarkdownDocumentBufferError> {
        self.flush()?;
        let replacement = match self.storage {
            OutputStorage::Inline(text) => CellReplacement::Inline(text),
            OutputStorage::File(file) => match file.freeze().map_err(edit_snapshot_error)? {
                Some(backing) if backing.len() <= EDIT_SEGMENT_BYTES => {
                    // A long raw draft may trim to a small result. Retain only
                    // this bounded result, actually returning its temporary mapping
                    // before handing heap bytes to the original commit receipt.
                    let observer = backing.observer();
                    claim.observe_backing(observer.clone()); // Bind first so actual read/final-release failure remains observed.
                    let copied = (|| {
                        let mut bytes = Vec::with_capacity(backing.len());
                        while bytes.len() < backing.len() {
                            let end = (bytes.len() + ORIGINAL_PIECE_BYTES).min(backing.len());
                            let lease = backing
                                .lease(bytes.len()..end)
                                .map_err(|error| edit_read_error(error.into(), false))?;
                            bytes.extend_from_slice(&lease);
                            lease
                                .finish()
                                .map_err(|error| edit_read_error(error.into(), false))?;
                        }
                        String::from_utf8(bytes)
                            .map_err(|_| MarkdownDocumentBufferError::InvalidUtf8Document)
                    })();
                    drop(backing);
                    let text = copied?; // Preserve actual read failure over later cleanup, without poisoning raw Source.
                    if let Some(errno) = observer.first_errno() {
                        return Err(MarkdownDocumentBufferError::EditStorageUnavailable(errno));
                    }
                    if !observer.unmap_succeeded() {
                        // Deliver Inline only after actual final munmap succeeds.
                        // An unreleased mapping cannot masquerade as a finished small result.
                        return Err(MarkdownDocumentBufferError::EditStorageUnavailable(
                            libc::EIO,
                        ));
                    }
                    claim.release_backing_ticket(); // Temporary mapping is actually released; Inline transfers no file obligation to the document.
                    CellReplacement::Inline(text)
                }
                Some(backing) => {
                    claim.observe_backing(backing.observer()); // The large candidate owns this mapping until failed release or canonical publication takes over.
                    CellReplacement::Frozen(Arc::new(backing))
                }
                None => CellReplacement::Inline(String::new()), // Empty global-trim output has no mapping or zero-length file leaf.
            },
        };
        Ok((replacement, self.utf16))
    }

    /// Only the actual File branch owns buffered bytes. Failure discards the
    /// complete candidate rather than resuming a partially written output.
    fn flush(&mut self) -> Result<(), MarkdownDocumentBufferError> {
        if self.scratch_len == 0 {
            return Ok(());
        }
        let OutputStorage::File(file) = &mut self.storage else {
            unreachable!("有限String快路不持文件缓冲");
        };
        file.write_all(&self.scratch[..self.scratch_len])
            .map_err(edit_snapshot_error)?;
        self.scratch_len = 0;
        Ok(())
    }
}
