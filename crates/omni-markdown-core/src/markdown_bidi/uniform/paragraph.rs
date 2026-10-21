//! A multi-paragraph level-zero certificate retains only the currently consumed paragraph.
//! Actual B endpoints still come from a bounded cursor on the same frozen tree.

use super::*;
use crate::markdown_document_buffer::FrozenBidiParagraphCursor;

#[derive(Default)]
pub(in crate::markdown_bidi) struct PreparedParagraph {
    known: Option<(usize, Range<usize>, usize)>,
    cursor: Option<FrozenBidiParagraphCursor>,
}

pub(crate) struct ParagraphPrepareStep {
    pub(crate) bytes: usize,
    pub(crate) scalars: usize,
    pub(crate) metadata: usize,
    pub(crate) ready: bool,
    pub(crate) maximum_borrow: usize,
}

impl ParagraphPrepareStep {
    pub(in crate::markdown_bidi) fn ready() -> Self {
        Self {
            bytes: 0,
            scalars: 0,
            metadata: 0,
            ready: true,
            maximum_borrow: 0,
        }
    }
}

impl PreparedParagraph {
    pub(in crate::markdown_bidi) fn range_at(&self, position: usize) -> Option<Range<usize>> {
        self.known
            .as_ref()
            .filter(|known| known.0 == position)
            .map(|known| known.1.clone())
    }

    /// A proven paragraph range may cover another position; the caller must separately
    /// establish that the target is a valid scalar/GCB boundary.
    pub(in crate::markdown_bidi) fn range_covering(&self, position: usize) -> Option<Range<usize>> {
        self.known
            .as_ref()
            .filter(|(_, range, _)| range.contains(&position))
            .map(|(_, range, _)| range.clone())
    }

    pub(in crate::markdown_bidi) fn control_free(
        &self,
        range: &Range<usize>,
    ) -> Option<(Range<usize>, usize)> {
        // Reuse only the prepared paragraph's real content interval, excluding its
        // trailing paragraph control. A subrange does not prove any new source endpoint.
        self.known
            .as_ref()
            .filter(|(_, paragraph, content_end)| {
                paragraph.start <= range.start && range.end <= *content_end
            })
            .map(|(_, paragraph, content_end)| (paragraph.clone(), *content_end))
    }

    pub(in crate::markdown_bidi) fn step(
        &mut self,
        snapshot: &MarkdownTextSnapshot,
        position: usize,
        budget: MarkdownBidiPrepareBudget,
    ) -> Result<ParagraphPrepareStep, BidiError> {
        if self.range_at(position).is_some() {
            return Ok(ParagraphPrepareStep::ready());
        }
        if self.range_covering(position).is_some() && snapshot.utf16_boundaries_are_scalar()? {
            self.known.as_mut().expect("已知真实段").0 = position;
            return Ok(ParagraphPrepareStep::ready()); // With no surrogate pairs, the new integer endpoint needs no repeated B-leaf read.
        }
        let retained = if let Some(cursor) = &mut self.cursor {
            cursor.position() == position || cursor.advance_position(snapshot, position)?
        } else {
            false
        };
        if !retained {
            self.cursor = Some(snapshot.begin_bidi_paragraph(position)?);
        } // Reuse only a real forward position in the same Frozen B scope; other requests retain the original cold path.
        let step = snapshot.advance_bidi_paragraph(
            self.cursor.as_mut().expect("原Frozen游标已安装"),
            budget.bytes,
            budget.scalars,
            budget.metadata_ops,
        )?;
        let ready = step.range.is_some();
        if let Some(range) = step.range {
            self.known = Some((position, range, step.content_end));
            self.cursor = self
                .cursor
                .take()
                .and_then(FrozenBidiParagraphCursor::into_forward_range);
            // Retain only this query's B range and scalar cursor point, avoiding another
            // endpoint search for the next GCB. Release the projection immediately: no
            // text lease, historical paragraphs, or extra Source owner survives this step.
        }
        Ok(ParagraphPrepareStep {
            bytes: step.bytes,
            scalars: step.scalars,
            metadata: step.metadata,
            ready,
            maximum_borrow: step.maximum_borrow,
        })
    }
}
