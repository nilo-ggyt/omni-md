//! A preparation turn consumes its original request and revalidates that ticket after Native reentry.
//! A successor request can never authorize publication for the previous Source.

use std::rc::Weak;

use omni_markdown_core::{MarkdownLayoutTextAuthority, MarkdownLayoutTextSource};

use super::{
    NativeMarkdownPagedBody as Owner, NativeMarkdownPagedBodyError as Error,
    NativeMarkdownPagedSceneRequest, Result, ViewError,
};

/// Immutable authority retained by one preparation attempt; it owns no replacement Source.
pub(super) struct PreparationTicket {
    pub(super) request: NativeMarkdownPagedSceneRequest,
    source: MarkdownLayoutTextAuthority,
    serial: u64,
    width: i64,
    padding: f64,
}

impl PreparationTicket {
    /// Freeze the Source, input, layout width, and owner epoch before preparation starts.
    pub(super) fn new(
        request: &NativeMarkdownPagedSceneRequest,
        source: &MarkdownLayoutTextSource,
        serial: u64,
        width: i64,
        padding: f64,
    ) -> Self {
        Self {
            request: request.clone(),
            source: source.authority(),
            serial,
            width,
            padding,
        }
    }

    /// Check Rust-owned facts without crossing a reentrant Objective-C boundary.
    pub(super) fn pure_current(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> bool {
        owner.current_body()
            && owner.serial.get() == self.serial
            && source.authority() == self.source
            && source.generation() == self.request.input.generation
            && source.utf16_len() == self.request.input.utf16_len
            && owner
                .input
                .target_domain()
                .is_ok_and(|target| target.same(&self.request.target))
            && Weak::ptr_eq(&self.request.document, &owner.input.document_identity())
            && owner
                .input
                .state()
                .is_ok_and(|state| state == self.request.input)
            && super::producer::width(&self.request, self.padding)
                .is_ok_and(|width| width == self.width)
    }

    /// Verify the original geometry at the Native boundary, then recheck pure facts.
    pub(super) fn current(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> bool {
        if !self.pure_current(owner, source) {
            return false; // Changed foreground facts revoke this ticket before any Native getter can reenter.
        }
        let Ok(view) = owner.view() else {
            return false;
        };
        let same = view
            .capture_scene_request()
            .is_ok_and(|actual| actual.same(&self.request));
        drop(view); // Release the temporary Native claim before rechecking; deallocation cannot renew this ticket.
        same && self.pure_current(owner, source)
    }

    /// Preserve the original mapping failure ahead of ordinary foreground revocation.
    pub(super) fn require(&self, owner: &Owner, source: &MarkdownLayoutTextSource) -> Result<()> {
        // Checking endpoint zero touches no document-wide text and retains the mapping
        // failure on both sides of potentially reentrant Native validation.
        source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        let current = self.current(owner, source);
        source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        if !current {
            return Err(Error::PreparationRevoked);
        }
        Ok(())
    }

    /// Guard a bounded Core quantum that cannot call Native code or reenter the UI.
    pub(super) fn require_pure(
        &self,
        owner: &Owner,
        source: &MarkdownLayoutTextSource,
    ) -> Result<()> {
        source
            .byte_offset_for_utf16(0)
            .map_err(ViewError::Document)?;
        if !self.pure_current(owner, source) {
            return Err(Error::PreparationRevoked);
        }
        Ok(()) // Pure Core work does not repeat all Native getters for each 256-scalar quantum.
    }
}

impl Error {
    /// Classify only errors that may safely yield to a successor foreground request.
    pub(super) fn is_foreground_revocation(&self) -> bool {
        use crate::native_surface::{
            paged_font_cascade::NativeMarkdownFontCascadeError as Cascade,
            paged_font_fallback::NativeMarkdownFontFallbackError as Fallback,
            paged_font_geometry::NativeMarkdownPagedFontGeometryError as Metrics,
            paged_fonts::NativeMarkdownPagedFontError as Capture,
            paged_line_fonts::NativeMarkdownLineFontError as Font,
            paged_viewport::NativeMarkdownPagedViewportError as Viewport,
        };
        let cascade_revoked =
            |cause: &Cascade| matches!(cause, Cascade::Revoked | Cascade::Font(Capture::Revoked));
        matches!(
            self,
            Self::PreparationRevoked
                | Self::View(ViewError::Revoked)
                | Self::Font(Capture::Revoked)
                | Self::Fallback(Fallback::Revoked)
                | Self::Registration(Font::Revoked | Font::Metrics(Metrics::Revoked))
                | Self::Viewport(Viewport::Revoked | Viewport::Busy)
        ) || matches!(self, Self::Fallback(Fallback::Cascade(cause) | Fallback::CandidateCapture { error: cause, .. }) if cascade_revoked(cause))
        // I/O faults, actual font failures, and InvalidRange remain real failures,
        // rather than being hidden by an ordinary successor request.
    }
}
