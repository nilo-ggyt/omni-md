//! Preserve actual scalar-level bidi shaping seams separately from grapheme
//! boundaries used by editing and line breaking.

use super::*;
use crate::markdown_bidi::MarkdownBidiShapingUnit;

/// Construct only from adjacent records actually read by the same bidi owner,
/// never public coordinate ranges. Retain no strong bidi/Source/directory edge;
/// original Source authority and request nonce still govern later admission.
#[derive(Clone)]
pub(crate) struct ResolvedBidiShapingSeam {
    source: MarkdownLayoutTextAuthority,
    byte: usize,
    utf16: usize,
    left_level: u8,
    right_level: u8,
}

impl ResolvedBidiShapingSeam {
    /// Seal only adjacent actual scalar heads with different resolved levels;
    /// a low surrogate never has this independent shaping-seam authority.
    pub(super) fn from_adjacent_units(
        source: &MarkdownLayoutTextAuthority,
        left: &MarkdownBidiShapingUnit,
        right: &MarkdownBidiShapingUnit,
    ) -> Result<Self> {
        if !left.permits(source) || !right.permits(source) || !left.precedes_different_level(right)
        {
            // Neither foreign records nor nonadjacent/equal-level units prove
            // the original natural bidi shaping boundary.
            return Err(MarkdownLineComposeError::InconsistentCoverage);
        }
        Ok(Self {
            source: source.clone(),
            byte: right.byte,
            utf16: right.utf16,
            left_level: left.level,
            right_level: right.level,
        })
    }

    /// Return original byte/UTF-16 scalar coordinates. This is still not a
    /// grapheme certificate and cannot authorize a caret or line-end boundary.
    pub(crate) fn boundary(&self, source: &MarkdownLayoutTextAuthority) -> Result<(usize, usize)> {
        if &self.source != source {
            // Equal integer coordinates cannot transfer this seam to another source.
            return Err(MarkdownLineComposeError::SourceMismatch);
        }
        Ok((self.byte, self.utf16))
    }

    /// Preserve both actual bidi levels; combining equal-parity directions must
    /// not rewrite the original visual-order facts at this boundary.
    pub(crate) fn levels(&self) -> (u8, u8) {
        (self.left_level, self.right_level)
    }
}

/// Both endpoint kinds authorize local shaping coordinates; only Grapheme also
/// retains the original editing-boundary fact. An additional seam excludes
/// different-level context even when it coincides with a grapheme boundary.
#[cfg(feature = "paged-shaping-frontier")]
pub(crate) enum CertifiedShapingEndpoint {
    Grapheme {
        proof: crate::markdown_layout_breaks::MarkdownCertifiedGraphemeEndpoint,
        seam: Option<ResolvedBidiShapingSeam>,
    },
    Bidi(ResolvedBidiShapingSeam),
}

#[cfg(feature = "paged-shaping-frontier")]
impl CertifiedShapingEndpoint {
    /// Return coordinates only. Consumers must retain this original enum and
    /// cannot register a new grapheme record from the returned coordinate structure.
    pub(crate) fn boundary(
        &self,
        source: &MarkdownLayoutTextAuthority,
    ) -> std::result::Result<crate::MarkdownGraphemeBoundary, MarkdownLayoutBreakError> {
        match self {
            Self::Grapheme { proof, seam } => {
                // An attached bidi barrier must agree with the original grapheme
                // endpoint in both coordinate spaces, not just its UTF-16 offset.
                let point = proof.boundary(source)?;
                if let Some(seam) = seam
                    && seam
                        .boundary(source)
                        .map_err(|_| MarkdownLayoutBreakError::SourceMismatch)?
                        != (point.byte, point.utf16)
                {
                    return Err(MarkdownLayoutBreakError::SourceMismatch);
                }
                Ok(point)
            }
            Self::Bidi(seam) => {
                // The shape API consumes this coordinate value without upgrading
                // its bidi-only proof into a grapheme editing certificate.
                let (byte, utf16) = seam
                    .boundary(source)
                    .map_err(|_| MarkdownLayoutBreakError::SourceMismatch)?;
                Ok(crate::MarkdownGraphemeBoundary { byte, utf16 })
            }
        }
    }

    /// Require both endpoints to belong to the original Source; two grapheme
    /// proofs must also share their original break-job scope. Bidi seams remain
    /// under the original style nonce/cancellation scope without impersonating a GCB ticket.
    pub(crate) fn require_pair(
        &self,
        other: &Self,
        source: &MarkdownLayoutTextAuthority,
    ) -> std::result::Result<(), MarkdownLayoutBreakError> {
        self.boundary(source)?;
        other.boundary(source)?;
        if let (Self::Grapheme { proof: left, .. }, Self::Grapheme { proof: right, .. }) =
            (self, other)
            && !left.same_scope(right)
        {
            // Two independent grapheme jobs cannot combine equal coordinates
            // into a certified pair belonging to either original job.
            return Err(MarkdownLayoutBreakError::SourceMismatch);
        }
        Ok(())
    }

    /// Borrow the original bidi context barrier regardless of endpoint representation.
    pub(crate) fn seam(&self) -> Option<&ResolvedBidiShapingSeam> {
        match self {
            Self::Grapheme { seam, .. } => seam.as_ref(),
            Self::Bidi(seam) => Some(seam),
        }
    }

    /// Distinguish actual grapheme proof from bidi-only shaping coordinates.
    pub(crate) fn is_grapheme(&self) -> bool {
        matches!(self, Self::Grapheme { .. })
    }
}
