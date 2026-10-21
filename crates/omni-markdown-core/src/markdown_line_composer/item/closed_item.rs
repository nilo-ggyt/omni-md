//! Single-use proofs for a naturally closed, bounded shaping item. An open prefix or
//! a budget-sized slice cannot establish a complete layout range.

use super::*;
use crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint;

mod fonts;
pub(super) use fonts::{
    FontRuns, advance_fonts, inspect_shaped, lend_font_item, provide_font_style,
};

// The UTF-16 bound also bounds the scalar count. Byte coordinates come from the
// original grapheme proofs; admission neither rereads text nor relocates endpoints.
pub(super) const MAX_CLOSED_UTF16: usize = 256;
pub(super) const MAX_CLOSED_BYTES: usize = 1024;

/// Issued only by the original Style phase. This non-Clone ticket retains weak
/// endpoint scopes, so public coordinates alone cannot replay its authority.
pub(crate) struct ClosedItem {
    source: MarkdownLayoutTextAuthority,
    font: MarkdownLayoutFontAuthority,
    scope: std::sync::Weak<()>,
    endpoints: [CertifiedShapingEndpoint; 2],
    cancellation: MarkdownShapingCancellation,
}

/// The sole shaping input obtained by consuming a closed-item ticket. Named ranges
/// distinguish item UTF-16, item bytes, and context; context is not a grapheme proof.
/// This value owns no Source or font payload. The immediately created original
/// PendingClosedShaping remains responsible for those real resources.
pub(crate) struct ClosedShapingInput {
    pub(crate) source_utf16: Range<usize>,
    pub(crate) source_bytes: Range<usize>,
    pub(crate) context_bytes: Range<usize>,
    pub(crate) cancellation: MarkdownShapingCancellation,
}

pub(super) enum Admission {
    Closed(ClosedItem),
    Frontier(CertifiedShapingEndpoint),
}

/// The existing item slot retains bounded preparation until the original Job is
/// assembled, then transfers the same font, style, paragraph facts, and seam proofs.
pub(super) struct PendingItem {
    pub(super) shaping: crate::markdown_shaping::PendingClosedShaping,
    pub(super) font: MarkdownLayoutFontAuthority,
    pub(super) registration: Option<MarkdownLineFontRegistration>,
    pub(super) replay_style: MarkdownLineComposeStyle,
    pub(super) facts: MarkdownLineItemFacts,
    pub(super) shaping_seams: [Option<ResolvedBidiShapingSeam>; 2],
}

impl ClosedItem {
    /// Accept a real font transition whose right edge is certified by font choice
    /// or the natural item endpoint. An arbitrary shorter response has no such proof.
    pub(super) fn from_terminal_font_change(
        terminal: &crate::markdown_shaping::frontier::MarkdownShapingTerminalGrapheme,
        endpoints: [MarkdownCertifiedGraphemeEndpoint; 2],
        authority: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
    ) -> Result<Self> {
        let range = terminal.source_range();
        let bytes = terminal.byte_range();
        let first = endpoints[0].boundary(authority)?;
        let last = endpoints[1].boundary(authority)?;
        // The terminal's complete blocking grapheme must remain inside the two
        // original endpoints. Neither a byte budget nor a foreign scope can close it.
        if terminal.source_authority() != authority
            || terminal.original_font().same_payload(font)
            || !endpoints[0].same_scope(&endpoints[1])
            || first.utf16 != range.start
            || last.utf16 < range.end
            || last.utf16 > terminal.item_end()
            || first.byte != bytes.start
            || last.byte < bytes.end
            || last.utf16 - first.utf16 > MAX_CLOSED_UTF16
            || last.byte - first.byte > MAX_CLOSED_BYTES
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        let scope = terminal.owner_scope();
        // A terminal outliving its producing owner cannot authorize another Job.
        if scope.strong_count() == 0 {
            return Err(MarkdownLineComposeError::Poisoned);
        }
        Ok(Self {
            source: authority.clone(),
            font: font.clone(),
            scope,
            endpoints: endpoints
                .map(|proof| CertifiedShapingEndpoint::Grapheme { proof, seam: None }),
            cancellation: terminal.cancellation(),
        })
    }

    pub(super) fn admit(
        payload: &mut Payload,
        authority: &MarkdownLayoutTextAuthority,
        response: &MarkdownLineStyleResponse,
        end: CertifiedShapingEndpoint,
        cancellation: &MarkdownLineComposeCancellation,
    ) -> Result<Admission> {
        // Check the original Source before inspecting coordinates or moving proofs;
        // a stale backing must fail rather than become an ordinary pending frontier.
        payload.source.health()?;
        let request = payload
            .request
            .as_ref()
            .ok_or(MarkdownLineComposeError::InvalidRequest)?;
        if payload.open_script || response.coverage != request.range {
            // An open Script_Extensions prefix or shortened response still needs
            // the original frontier to establish the natural item end.
            return Ok(Admission::Frontier(end));
        }
        let scan = payload
            .scan
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let start = scan.endpoint_proofs[0]
            .as_ref()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        let first = start.boundary(authority)?;
        let last = end.boundary(authority)?;
        start.require_pair(&end, authority)?;
        // The actual Style request, scanner, Source, and paired endpoint proofs
        // must describe one complete item. A mismatch is never downgraded to Pending.
        if !matches!(payload.stage, Stage::Style)
            || !Arc::ptr_eq(&request.nonce, &response.request.nonce)
            || request.source != *authority
            || !payload.source.permits(authority)
            || scan.start != request.range.start
            || scan.next != request.range.end
            || first.utf16 != request.range.start
            || last.utf16 != request.range.end
            || first.byte >= last.byte
            || last.byte > payload.source.byte_len()
        {
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        if request.range.len() > MAX_CLOSED_UTF16 || last.byte - first.byte > MAX_CLOSED_BYTES {
            // A genuinely large item continues through the same frontier; retain
            // its complete style and paragraph instead of manufacturing a short item.
            return Ok(Admission::Frontier(end));
        }
        let scope = Arc::downgrade(
            payload
                .font_probe_scope
                .as_ref()
                .ok_or(MarkdownLineComposeError::InconsistentCoverage)?,
        );
        // Recheck before consuming the original left proof. Failed admission leaves
        // that proof in the scanner; success moves it without another reader/index.
        payload.source.health()?;
        let start = payload
            .scan
            .as_mut()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?
            .endpoint_proofs[0]
            .take()
            .ok_or(MarkdownLineComposeError::InconsistentCoverage)?;
        Ok(Admission::Closed(Self {
            source: authority.clone(),
            font: response.font.authority(),
            scope,
            endpoints: [start, end],
            cancellation: cancellation.shape.clone(),
        }))
    }

    /// Consume the original ticket, style scope, and cancellation authority. Bare
    /// coordinates cannot enter this conversion or renew an expired producing scope.
    pub(crate) fn into_input(
        self,
        source: &MarkdownLayoutTextAuthority,
        font: &MarkdownLayoutFontAuthority,
        source_bytes: usize,
    ) -> std::result::Result<ClosedShapingInput, MarkdownShapingError> {
        if &self.source != source {
            return Err(MarkdownShapingError::SourceMismatch);
        } // Equal text and coordinates in another Source do not inherit this proof.
        if !self.font.same_payload(font) {
            return Err(MarkdownShapingError::FontMismatch);
        } // A same-named font cannot replace the payload selected by the response.
        if self.scope.strong_count() == 0 {
            return Err(MarkdownShapingError::Poisoned);
        } // Retained coordinates cannot authorize a Job after the Style owner ends.
        let [start, end] = self.endpoints;
        let first = start
            .boundary(source)
            .map_err(|_| MarkdownShapingError::SourceMismatch)?;
        let last = end
            .boundary(source)
            .map_err(|_| MarkdownShapingError::SourceMismatch)?;
        // Endpoint access checks identity and weak scopes without I/O; the assembly
        // entry is responsible for checking the actual Source's health first.
        start
            .require_pair(&end, source)
            .map_err(|_| MarkdownShapingError::SourceMismatch)?;
        // A certified bidi seam stops shaping context at that endpoint. Without
        // such a seam, keep the original Source extent available to bounded readers.
        let context = (if start.seam().is_some() {
            first.byte
        } else {
            0
        })..(if end.seam().is_some() {
            last.byte
        } else {
            source_bytes
        });
        Ok(ClosedShapingInput {
            source_utf16: first.utf16..last.utf16,
            source_bytes: first.byte..last.byte,
            context_bytes: context,
            cancellation: self.cancellation,
        })
    }
}

#[cfg(test)]
mod tests;
