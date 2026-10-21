//! Expose only a sealed local-read capsule. There is no public conversion into a
//! canonical Source or escape hatch returning an unrestricted ordinary snapshot.

use super::*;

struct ReadPayload {
    // The real read claim is Frozen's final field. Internal snapshot clones must
    // not return that claim merely because this outer payload has dropped.
    snapshot: MarkdownTextSnapshot,
    facts: MarkdownCellDraftFacts,
    scope: Arc<Scope>,
}

#[derive(Clone)]
/// Clones share one actual reader payload. Editing/cancellation revokes further
/// reads without mutating immutable bytes already held by their original owner.
pub struct MarkdownCellDraftReadCapsule {
    payload: Arc<ReadPayload>,
}

#[derive(Clone)]
/// Weak identity of this exact capture. Coordinates, equal length, or an ordinary
/// Source receipt cannot manufacture the same authority.
pub struct MarkdownCellDraftReadAuthority {
    payload: Weak<ReadPayload>,
}

impl MarkdownDocumentBuffer {
    /// Freeze a read root for this draft's current content_serial. State-only
    /// changes keep its text valid; actual edits revoke the old read authority.
    pub fn capture_cell_draft(
        &self,
        token: &MarkdownCellDraft,
    ) -> Result<MarkdownCellDraftReadCapsule, MarkdownCellDraftError> {
        let pending = self.active_cell_draft(token)?;
        let claim = pending.scope.reserve_reader()?;
        let guard = FrozenDraftGuard::new(
            Arc::clone(&pending.scope),
            pending.content_serial,
            self.original_snapshot.clone(),
        );
        let snapshot = MarkdownTextSnapshot::capture_draft_root(
            self,
            &pending.root,
            &pending.edits,
            pending.content_serial,
            guard,
            claim,
        );
        // Even a candidate without Original leaves preserves original Source's
        // sticky I/O failure ahead of a cache hit or ordinary staleness.
        let payload = Arc::new(ReadPayload {
            snapshot,
            facts: pending.facts(),
            scope: Arc::clone(&pending.scope),
        });
        pending.require(self)?;
        Ok(MarkdownCellDraftReadCapsule { payload })
    }
}

impl MarkdownCellDraftReadCapsule {
    #[must_use]
    /// The receipt does not keep its capsule alive. Separate captures cannot
    /// exchange receipts even when their numeric serials are equal.
    pub fn authority(&self) -> MarkdownCellDraftReadAuthority {
        MarkdownCellDraftReadAuthority {
            payload: Arc::downgrade(&self.payload),
        }
    }

    fn require(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<(), MarkdownCellDraftError> {
        // Check actual frozen storage and original content revocation before weak
        // capture identity, so a foreign receipt cannot hide this Source's failure.
        self.payload
            .snapshot
            .original_backing_health()
            .inspect_err(|&error| self.payload.scope.record_source(error))
            .map_err(|error| match error {
                MarkdownDocumentBufferError::PreparedTextInputStale => {
                    MarkdownCellDraftError::Stale
                }
                other => other.into(),
            })?;
        self.payload
            .scope
            .require_content(self.payload.facts.content_serial)?;
        if !Weak::ptr_eq(&authority.payload, &Arc::downgrade(&self.payload)) {
            return Err(MarkdownCellDraftError::Stale);
        }
        Ok(())
    }

    fn finish<T>(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        result: Result<T, MarkdownDocumentBufferError>,
    ) -> Result<T, MarkdownCellDraftError> {
        if let Err(error) = &result {
            self.payload.scope.record_source(*error);
        }
        // Preserve original I/O failure first. Revocation during a callback still
        // prevents any previously observed partial candidate from escaping as success.
        self.require(authority)?;
        result.map_err(Into::into)
    }

    /// Return captured local facts only. Selection may predate current input state;
    /// these facts grant neither canonical-coordinate nor edit authority.
    pub fn facts(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<MarkdownCellDraftFacts, MarkdownCellDraftError> {
        self.require(authority)?;
        Ok(self.payload.facts.clone())
    }

    /// Return original cell-local text identity. Equal length, an ordinary Source,
    /// or a bare range cannot construct this weak receipt.
    pub fn source_authority(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<MarkdownCellDraftSourceAuthority, MarkdownCellDraftError> {
        self.require(authority)?;
        self.payload
            .snapshot
            .cell_draft_source_authority()
            .ok_or(MarkdownCellDraftError::Stale)
    }

    /// Only the controlled layout-Source factory may share this Frozen. The actual
    /// ReadClaim is returned only when the final snapshot owner has dropped.
    #[cfg(any(
        feature = "paged-graphemes",
        feature = "paged-line-breaks",
        feature = "paged-shaping"
    ))]
    pub(crate) fn layout_snapshot(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
    ) -> Result<MarkdownTextSnapshot, MarkdownCellDraftError> {
        self.require(authority)?;
        Ok(self.payload.snapshot.clone())
    }

    /// Read at most 4096 UTF-16 units; surrogate-half ranges remain valid NSString
    /// reads. On error destination may contain a prefix, so discard the whole candidate.
    pub fn copy_utf16_code_units(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        offset: usize,
        destination: &mut [u16],
    ) -> Result<(), MarkdownCellDraftError> {
        self.require(authority)?;
        if destination.len() > read_cache::READ_WINDOW_UTF16_UNITS {
            return Err(MarkdownCellDraftError::ReadLimit);
        }
        self.finish(
            authority,
            self.payload
                .snapshot
                .copy_utf16_code_units(offset, destination),
        )
    }

    /// Read at most 4 KiB per call through original finite pieces. Err forbids
    /// publishing any prefix already observed by the visitor.
    pub fn visit_byte_range(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        range: Range<usize>,
        visit: impl FnMut(&[u8]) -> Result<(), MarkdownDocumentBufferError>,
    ) -> Result<(), MarkdownCellDraftError> {
        self.require(authority)?;
        if range.end.saturating_sub(range.start) > ORIGINAL_PIECE_BYTES {
            return Err(MarkdownCellDraftError::ReadLimit);
        }
        self.finish(
            authority,
            self.payload.snapshot.visit_byte_range(range, visit),
        )
    }

    /// Resolve scalar coordinates through actual bounded leaves, rejecting
    /// positions inside surrogate pairs or UTF-8 encodings.
    pub fn byte_offset_for_utf16(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        offset: usize,
    ) -> Result<usize, MarkdownCellDraftError> {
        self.require(authority)?;
        self.finish(
            authority,
            self.payload.snapshot.byte_offset_for_utf16(offset),
        )
    }

    /// Translate an actual byte boundary into local UTF-16 without granting
    /// authority over any global document or table range.
    pub fn utf16_offset_for_byte(
        &self,
        authority: &MarkdownCellDraftReadAuthority,
        offset: usize,
    ) -> Result<usize, MarkdownCellDraftError> {
        self.require(authority)?;
        self.finish(
            authority,
            self.payload.snapshot.utf16_offset_for_byte(offset),
        )
    }
}
