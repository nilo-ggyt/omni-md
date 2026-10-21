//! Retain folding proof for the same original Source. Unicode jobs share its
//! real endpoints rather than building a second text representation.

use std::ops::Range;

use super::{MarkdownLineComposeError as Error, item::Result};
use crate::{
    MarkdownLayoutTextSource, markdown_folding::anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS,
    markdown_layout_source::folds::FoldPlan,
};

#[derive(Default)]
pub(super) struct Plan {
    shared: Option<FoldPlan>,
}

impl Plan {
    /// Retain at most the existing 256 explicit anchors. One shared plan proves
    /// actual Source paragraph boundaries, including CRLF and real EOF.
    pub(super) fn new(source: &MarkdownLayoutTextSource, ranges: &[Range<usize>]) -> Result<Self> {
        source.health()?;
        if ranges.len() > MAX_MARKDOWN_FOLD_ANCHORS {
            // Reject excess intent before constructing a directory or copying ranges.
            return Err(Error::InvalidRequest);
        }
        let shared = if ranges.is_empty() {
            // An unfolded document needs no additional fold-proof owner.
            None
        } else {
            // Validate every supplied range against this exact original Source.
            Some(FoldPlan::new(source, ranges)?)
        };
        Ok(Self { shared })
    }

    /// Script, grapheme, line-break, and bidi jobs share the same weak Source
    /// proof and finite cursors without duplicating its directory or recapturing text.
    pub(super) fn shared(&self) -> Option<FoldPlan> {
        self.shared.clone()
    }

    /// Issue a fold control only at an actual hidden-range start. A viewport
    /// edge cannot be used to infer authority over a different hidden interval.
    pub(super) fn at(&self, position: usize) -> Option<Range<usize>> {
        let spans = self.shared.as_ref()?.spans();
        let index = spans
            .binary_search_by_key(&position, |span| span.start.utf16)
            .ok()?;
        Some(spans[index].start.utf16..spans[index].end.utf16)
    }
}
