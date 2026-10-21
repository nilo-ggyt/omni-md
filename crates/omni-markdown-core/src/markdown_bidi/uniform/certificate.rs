//! Control facts sealed by a complete original-range proof: exact full-root
//! properties or a completed budgeted scan. An unfinished prefix is insufficient.

use super::super::{BidiError, MarkdownBidiCancellation};
use unicode_bidi::BidiClass;

#[cfg(feature = "paged-shaping-frontier")]
enum InlineControls {
    Absent,
    Present,
}

// The original preparation owns either exact full-root facts or scan observations.
// Downstream consumers can use them only after the complete proof is sealed.
pub(in crate::markdown_bidi) struct UniformCertificate {
    #[cfg(feature = "paged-shaping-frontier")]
    controls: InlineControls,
    #[cfg(feature = "paged-shaping-frontier")]
    paragraphs: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    interior_controls_absent: bool,
}

impl UniformCertificate {
    // Paragraph boundaries require the shaping frontier's completed source facts;
    // builds without that consumer do not infer new boundaries from this owner.
    pub(in crate::markdown_bidi) fn has_paragraph_boundaries(&self) -> bool {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            self.paragraphs
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        {
            false
        }
    }
    #[cfg(feature = "paged-shaping-frontier")]
    pub(super) fn from_properties(
        properties: crate::markdown_document_buffer::MarkdownUnicodeProperties,
    ) -> Self {
        Self {
            controls: if properties.separators() == 0 {
                InlineControls::Absent
            } else {
                // The summary proves exact presence over the complete original
                // range. TAB/LS can still be Uniform, but cannot authorize a
                // control-free ItemScan: UBA classes are not Markdown policy.
                InlineControls::Present
            },
            paragraphs: properties.bidi_classes()
                & crate::markdown_document_buffer::MarkdownUnicodeProperties::class_mask(
                    unicode_bidi::BidiClass::B,
                )
                != 0,
            // Of the six Markdown inline separators, only TAB and LS are not B.
            // The others can be excluded only at an actual B paragraph endpoint.
            // This says nothing about FF/VT/SHY being GCB_Control: grapheme and
            // font processing retain their independent responsibilities.
            interior_controls_absent: properties.separators() & ((1 << 0) | (1 << 4)) == 0,
        }
    }

    pub(super) fn clear() -> Self {
        // Empty facts alone do not authorize sealing. The caller must establish
        // an exact full-scope proof or complete the actual original-range scan.
        Self {
            #[cfg(feature = "paged-shaping-frontier")]
            controls: InlineControls::Absent,
            #[cfg(feature = "paged-shaping-frontier")]
            paragraphs: false,
            #[cfg(feature = "paged-shaping-frontier")]
            interior_controls_absent: true,
        }
    }

    pub(super) fn observe(&mut self, scalar: char, class: BidiClass) {
        #[cfg(feature = "paged-shaping-frontier")]
        {
            // The same complete reader has resolved this scalar's UBA class.
            // A trailing B remains part of the original paragraph: it does not
            // erase zero-level content facts or prove the whole range control-free.
            self.paragraphs |= class == BidiClass::B;
            // TAB and LS can occur inside a B paragraph and must keep blocking
            // control-free content authorization. Only the subsequent actual B
            // cursor's content_end can exclude CR/LF/NEL/PS; do not guess a suffix.
            self.interior_controls_absent &= !matches!(scalar, '\t' | '\u{2028}');
        }
        #[cfg(feature = "paged-shaping-frontier")]
        if matches!(
            scalar,
            '\t' | '\n' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'
        ) {
            // This exact set matches Markdown inline_control, not the UBA S/B
            // classes. U+2028 is WS yet has native line-breaking duties; other
            // S/B scalars do not automatically gain product control semantics.
            self.controls = InlineControls::Present;
        }
        #[cfg(not(feature = "paged-shaping-frontier"))]
        let _ = (scalar, class); // No itemization fast path means no extra facts or UBA changes.
    }

    pub(in crate::markdown_bidi) fn seal(
        self,
        cancellation: MarkdownBidiCancellation,
    ) -> UniformResolution {
        // Transfer completed facts and the same cancellation authority together.
        // A consumer must still check cancellation before using the resolution.
        UniformResolution {
            cancellation,
            #[cfg(feature = "paged-shaping-frontier")]
            controls: self.controls,
            #[cfg(feature = "paged-shaping-frontier")]
            paragraphs: self.paragraphs,
            #[cfg(feature = "paged-shaping-frontier")]
            interior_controls_absent: self.interior_controls_absent,
            #[cfg(feature = "paged-shaping-frontier")]
            paragraph: super::paragraph::PreparedParagraph::default(),
        }
    }
}

pub(in crate::markdown_bidi) struct UniformResolution {
    cancellation: MarkdownBidiCancellation,
    #[cfg(feature = "paged-shaping-frontier")]
    controls: InlineControls,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) paragraphs: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) interior_controls_absent: bool,
    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) paragraph: super::paragraph::PreparedParagraph,
}

impl UniformResolution {
    pub(in crate::markdown_bidi) fn check(&self) -> Result<(), BidiError> {
        // Completion does not detach a resolution from its original cancellation.
        self.cancellation.check()
    }

    #[cfg(feature = "paged-shaping-frontier")]
    pub(in crate::markdown_bidi) fn control_free(&self) -> bool {
        // Whole-range authorization requires both no paragraph boundary and no
        // Markdown inline separator; a control-free interior alone is weaker.
        // This structural predicate does not replace entry Source/cancel guards.
        !self.paragraphs && matches!(self.controls, InlineControls::Absent)
    }
}
