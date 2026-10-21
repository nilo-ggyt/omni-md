//! Character reads and Scene identity share one Body/Cell target; cell-local UTF-16 cannot enter Body consumers through the canonical document identity.

use super::cell::{CellPermit, NativeMarkdownPagedCellSession};
use super::*;
use omni_markdown_core::{
    MarkdownCellDraftSourceAuthority, MarkdownLayoutTextSource, MarkdownLayoutTextSourceDomain,
};

pub(super) enum InputTarget {
    Body(Weak<RefCell<MarkdownDocumentBuffer>>),
    Cell {
        session: Weak<NativeMarkdownPagedCellSession>,
        serial: Cell<u64>,
    },
}

#[derive(Clone)]
enum Domain {
    Body(Weak<RefCell<MarkdownDocumentBuffer>>),
    Cell {
        session: Weak<NativeMarkdownPagedCellSession>,
        source: MarkdownCellDraftSourceAuthority,
        alignment: omni_markdown_core::MarkdownTableAlignment,
    },
}

#[derive(Clone)]
/// An opaque domain captured only by Input: cells/roots in one document are not interchangeable through equal coordinates or versions.
pub struct NativeMarkdownPagedTargetDomain(Domain);

impl NativeMarkdownPagedTargetDomain {
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        // Body identity is its original allocation. Cell additionally binds the
        // draft Source and captured alignment, which equal local offsets omit.
        match (&self.0, &other.0) {
            (Domain::Body(a), Domain::Body(b)) => Weak::ptr_eq(a, b),
            (
                Domain::Cell {
                    session: a,
                    source: x,
                    alignment: left,
                },
                Domain::Cell {
                    session: b,
                    source: y,
                    alignment: right,
                },
            ) => Weak::ptr_eq(a, b) && x == y && left == right,
            _ => false, // Body and Cell never share coordinates; equal document identity cannot authorize a local range.
        }
    }

    #[must_use]
    pub fn is_body(&self, document: &Weak<RefCell<MarkdownDocumentBuffer>>) -> bool {
        matches!(&self.0, Domain::Body(original) if Weak::ptr_eq(original, document))
    }

    /// The captured Cell session/Source owns its column alignment; Body has no column alignment and retains paragraph settings.
    #[must_use]
    pub fn cell_alignment(&self) -> Option<omni_markdown_core::MarkdownTableAlignment> {
        match &self.0 {
            Domain::Body(_) => None,
            Domain::Cell { alignment, .. } => Some(*alignment),
        }
    }

    /// Check original capture kind and weak content identity here; the Body producer and SceneKey still verify the exact document/capture.
    ///
    /// # Errors
    /// Reject failed Source or a mismatched target domain; public ranges/generations cannot authorize a new Source.
    pub fn require_source(&self, source: &MarkdownLayoutTextSource) -> Result<()> {
        source.byte_offset_for_utf16(0)?; // The bounded public endpoint check preserves the Source first error without borrowing another Body or creating authority.
        // Read-only table-cell Source is a third domain, not a canonical Body or
        // active draft; neither input target can borrow its local coordinates.
        let accepted = match &self.0 {
            Domain::Body(_) => source.domain() == MarkdownLayoutTextSourceDomain::Canonical,
            Domain::Cell {
                session,
                source: original,
                ..
            } => {
                source.domain() == MarkdownLayoutTextSourceDomain::ActiveCellDraft
                    && session.strong_count() != 0
                    && source.cell_draft_authority().as_ref() == Some(original)
            }
        };
        if !accepted {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        source.byte_offset_for_utf16(0)?;
        Ok(())
    }
}

impl InputTarget {
    pub(super) fn body_document(&self) -> Weak<RefCell<MarkdownDocumentBuffer>> {
        match self {
            Self::Body(document) => document.clone(),
            Self::Cell { .. } => Weak::new(), // Body-only Find/Source consumers fail explicitly rather than apply a local range to canonical text.
        }
    }

    pub(super) fn cell(&self) -> Result<Option<Rc<NativeMarkdownPagedCellSession>>> {
        match self {
            Self::Body(_) => Ok(None),
            Self::Cell { session, .. } => session
                .upgrade()
                .map(Some)
                .ok_or(NativeMarkdownPagedInputError::Closed),
        }
    }

    pub(super) fn cell_permit(&self) -> Result<Option<CellPermit>> {
        self.cell()?.map(|session| session.capture()).transpose()
    }

    pub(super) fn domain(&self) -> Result<NativeMarkdownPagedTargetDomain> {
        let domain = match self {
            Self::Body(document) => Domain::Body(document.clone()),
            Self::Cell { session, serial } => {
                // Keep the original serial check: capturing a new Source after an
                // external draft mutation cannot renew this Input's target domain.
                let original = session
                    .upgrade()
                    .ok_or(NativeMarkdownPagedInputError::Closed)?;
                let (facts, source) = original.capture()?.facts()?;
                if facts.serial != serial.get() {
                    return Err(NativeMarkdownPagedInputError::StateChanged);
                }
                Domain::Cell {
                    session: session.clone(),
                    source,
                    alignment: original.alignment(),
                }
            }
        };
        Ok(NativeMarkdownPagedTargetDomain(domain))
    }
}

impl NativeMarkdownPagedInput {
    /// Capture layout Source only for this input permit: use the original Cell factory or canonical document capture without copying text or creating a draft.
    ///
    /// # Errors
    /// Release this capture if original permit/domain/content/Source is invalid; an original mapping failure precedes foreground revocation.
    pub fn capture_layout_source(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<MarkdownLayoutTextSource> {
        self.check_permit(permit, current)?;
        let domain = self.target.domain()?;
        let source = if let Some(cell) = &permit.cell {
            cell.session()?.capture_source()? // The original token authorizes local Source; canonical coordinates cannot manufacture Cell identity.
        } else {
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            MarkdownLayoutTextSource::capture(&document)
        };
        domain.require_source(&source)?;
        let checked = self.check_permit(permit, current);
        domain.require_source(&source)?; // current may reenter and change Cell; check the old Source health before returning generic revocation.
        checked?;
        if !self.target.domain()?.same(&domain) {
            return Err(NativeMarkdownPagedInputError::StateChanged);
        }
        Ok(source)
    }

    #[must_use]
    pub fn is_cell(&self) -> bool {
        matches!(self.target, InputTarget::Cell { .. })
    }
    /// Read at most 4096 UTF-16 code units under one permit, including partial surrogate pairs; Native string callers separately validate scalar endpoints.
    ///
    /// # Errors
    /// Discard the candidate when original Source/target/serial is invalid; Cell acquires no extra frozen root or layout reader.
    pub fn copy_utf16_code_units(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        offset: usize,
        destination: &mut [u16],
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<()> {
        self.check_permit(permit, current)?;
        if destination.len() > 4096 {
            return Err(NativeMarkdownPagedInputError::InvalidRange);
        }
        if let Some(cell) = &permit.cell {
            cell.with_edit(|document, edit| {
                Ok(document.copy_cell_draft_utf16_code_units(edit, offset, destination)?)
            })?;
        } else {
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            document.copy_utf16_code_units(offset, destination)?;
        }
        // Returned units are a candidate until original authority survives the
        // read. Callers discard the entire bounded destination on rejection.
        self.check_permit(permit, current)
    }

    /// Validate in the original target's local/global domain; never apply Cell coordinates to the canonical byte tree.
    ///
    /// # Errors
    /// Reject Source failure, revocation, bounds errors, or split scalars; even empty ranges and EOF check actual Source health.
    pub fn validate_utf16_range(
        &self,
        permit: &NativeMarkdownPagedInputPermit,
        range: Range<usize>,
        current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<()> {
        self.check_permit(permit, current)?;
        if let Some(cell) = &permit.cell {
            cell.with_edit(|document, edit| {
                Ok(document.validate_cell_draft_utf16_range(edit, range)?)
            })?;
        } else {
            let original = self
                .document_identity()
                .upgrade()
                .ok_or(NativeMarkdownPagedInputError::DocumentReleased)?;
            let document = original
                .try_borrow()
                .map_err(|_| NativeMarkdownPagedInputError::Borrowed)?;
            if range.start > range.end {
                document.byte_offset_for_utf16(0)?;
                return Err(NativeMarkdownPagedInputError::InvalidRange);
            }
            document.byte_offset_for_utf16(range.start)?;
            document.byte_offset_for_utf16(range.end)?;
        }
        self.check_permit(permit, current)
    }
}
