//! Borrow selection-style invariance only during synchronous handoff, without extending old publication or glyph-handle authority.

use super::*;

/// Selection movement within a proved Plain prefix or the same Root Code body leaves the proved styles unchanged.
/// Joint borrows of the request, document, and last publication prevent retaining this proof across edits or reprojection.
/// This capability proves no grapheme boundary, soft line, or editable range and does not replace publication for the new selection.
pub struct MarkdownSelectionStyleInvariant<'a> {
    request: &'a MarkdownViewportSourceRequest,
    document: &'a MarkdownDocumentBuffer,
    previous: &'a VerifiedMarkdownViewportSources,
    authority: MarkdownViewportSourceAuthority,
    kind: InvariantKind<'a>,
}

/// The original producer supplies a closed Root Code owner with no selection-sensitive inline markers or quote/list prefixes.
/// Other structural and underline owners contain both body endpoints or neither, so this case covers the same Source's captured glyph store.
/// Plain remains restricted to the scanner's actually read prefix; the Code case grants no authority over an unknown paragraph suffix.
enum InvariantKind<'a> {
    PlainPrefix {
        utf16_end: usize,
    },
    RootCode {
        leaf: &'a VerifiedMarkdownOrdinaryLeaf,
    },
}

impl MarkdownViewportSourceRequest {
    /// Lend the actual invariant so the same Source's Composer can check its captured glyphs and supported domain.
    /// None changes nothing; success also leaves the old publication live, and the caller must immediately use reproject_selection.
    ///
    /// # Errors
    /// Preserve source failure, cancellation, foreign document/generation, complete-selection mismatch, and stale publication errors.
    pub fn selection_style_invariant<'a>(
        &'a self,
        document: &'a MarkdownDocumentBuffer,
        previous: &'a VerifiedMarkdownViewportSources,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<Option<MarkdownSelectionStyleInvariant<'a>>, MarkdownTableRequestError> {
        self.require_selection_invariant_publication(document, previous, authority)?;
        if !self.leaf_mode
            || self.job.is_some()
            || self.coordinate_active
            || self.cell_inline.is_some()
            || self.row_source_position.is_some()
        {
            return Ok(None); // Active jobs still own their cleanup obligations; a new selection cannot truncate their work.
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = &previous.semantic else {
            return Ok(None);
        };
        if previous.table.windows_for(document)?.next().is_some()
            || !previous.table.structural_tables().is_empty()
        {
            return Ok(None); // Table windows have their own projection owners; ordinary leaves cannot fill the hull between them.
        }
        let kind = if let Some(code) = leaf.root_code_leaf() {
            let body = code.root_code_body.as_ref().unwrap();
            if code.block.source_revision() != self.source.generation()
                || code.block.source_end() != self.source.byte_len()
                || !body.contains_selection(self.selection)
                || !body.contains_selection(authority.selection)
            {
                return Ok(None);
            }
            // The no-table fact admits reprojection of this finite window through the original batch.
            // Store-wide style invariance comes from the real Root Code owner, not a fabricated document-wide gap.
            let mut covered = false;
            for gap in previous.table.gaps_for(document)? {
                let range = gap.utf16_range_for(document)?;
                covered |= range.start <= self.viewport.start && self.viewport.end <= range.end;
            }
            if !covered {
                return Ok(None);
            }
            InvariantKind::RootCode { leaf: code }
        } else {
            let Some(end) = self
                .semantic
                .as_ref()
                .and_then(|job| job.selection_invariant_prefix(&self.source, leaf))
            else {
                return Ok(None);
            };
            let mut covered = false;
            for gap in previous.table.gaps_for(document)? {
                let bytes = gap.byte_range_for(document)?;
                let utf16 = gap.utf16_range_for(document)?;
                covered |= bytes.start == 0
                    && end.byte <= bytes.end
                    && utf16.start == 0
                    && end.utf16 <= utf16.end;
            }
            if !covered {
                return Ok(None); // An empty directory proves no absence; Plain requires the original gap to cover the whole read prefix.
            }
            InvariantKind::PlainPrefix {
                utf16_end: end.utf16,
            }
        };
        Ok(Some(MarkdownSelectionStyleInvariant {
            request: self,
            document,
            previous,
            authority,
            kind,
        }))
    }

    /// The old publication retains its complete old selection while the document has the new one; this explicit handoff checks both.
    /// These checks only observe original owners, without Native callbacks, source leases, or another lifecycle state.
    fn require_selection_invariant_publication(
        &self,
        document: &MarkdownDocumentBuffer,
        previous: &VerifiedMarkdownViewportSources,
        authority: MarkdownViewportSourceAuthority,
    ) -> Result<(), MarkdownTableRequestError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        document
            .byte_offset_for_utf16(0)
            .map_err(MarkdownTableRequestError::Source)?;
        check_authority(
            &self.source,
            self.context_revision,
            authority.selection,
            document,
            authority,
        )?;
        previous.publication.require_live()?;
        if !matches!(self.phase, SourcePhase::Consumed)
            || self.context_revision != previous.publication.context_revision
            || self.selection != previous.publication.selection
            || !self
                .last_publication
                .as_ref()
                .is_some_and(|last| last.ptr_eq(&Rc::downgrade(&previous.publication)))
        {
            return Err(MarkdownTableRequestError::PublicationSuperseded);
        }
        Ok(())
    }
}

impl MarkdownSelectionStyleInvariant<'_> {
    /// Let Composer consume a proved endpoint for the same Canonical Source, without turning a naked range into Native authority.
    /// Immutable borrows prevent same-stack selection changes or request retirement; recheck original health immediately before consumption.
    pub(crate) fn end_for(
        &self,
        source: &crate::MarkdownLayoutTextSource,
    ) -> Result<usize, MarkdownTableRequestError> {
        source.health().map_err(MarkdownTableRequestError::Source)?;
        self.request.require_selection_invariant_publication(
            self.document,
            self.previous,
            self.authority,
        )?;
        if source.domain() != crate::MarkdownLayoutTextSourceDomain::Canonical
            || !source
                .snapshot_for_bidi()
                .shares_current_table_root(self.document)
        {
            return Err(MarkdownTableRequestError::SourceSuperseded);
        }
        match &self.kind {
            InvariantKind::PlainPrefix { utf16_end } => Ok(*utf16_end),
            InvariantKind::RootCode { leaf } => {
                // The real closed leaf binds this wider capability to the original document end.
                // Composer still checks its supported domain and original Font/Source health; no global boolean substitutes for the leaf.
                if leaf.block.source_end() != self.document.byte_len() {
                    return Err(MarkdownTableRequestError::SourceSuperseded);
                }
                Ok(self.document.utf16_len())
            }
        }
    }
}
