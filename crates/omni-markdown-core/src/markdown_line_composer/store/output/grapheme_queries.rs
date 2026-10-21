//! Grapheme coverage belongs to the read-only snapshot independently of glyph
//! layout coverage. Untransferred suffix records remain Pending, never false EOF.
use super::*;

/// Known(None) means no GCB in the covered ordinal domain. Hidden ranges produce
/// typed rejection; Pending means the original records have not transferred yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedGraphemeOrdinalAnswer {
    Pending,
    Known(Option<usize>),
}

impl MarkdownComposedLines {
    /// Ordinals belong to this explicit record domain. FoldedVisible excludes
    /// internal hidden boundaries and cannot supply a complete Source GCB count.
    pub fn grapheme_ordinal_domain(&self) -> Result<crate::MarkdownGraphemeOrdinalDomain> {
        self.require(&self.authority)?;
        Ok(self.graphemes.domain)
    }

    /// Establish dimensions from the same healthy Source, rejecting domains
    /// whose internal boundaries are intentionally absent from this directory.
    fn grapheme_dimensions(&self, offset: usize) -> Result<(usize, usize)> {
        self.require(&self.authority)?;
        let source = &self.payload.as_ref().ok_or(Error::Poisoned)?.source;
        if offset > source.utf16_len() {
            // EOF is valid; a coordinate beyond actual Source is invalid input.
            return Err(Error::InvalidRequest);
        }
        if let Some(fold) = self
            .payload
            .as_ref()
            .and_then(|payload| payload.folds.as_ref())
            .and_then(|plan| {
                plan.spans()
                    .iter()
                    .find(|fold| fold.start.utf16 < offset && offset < fold.end.utf16)
            })
        {
            return Err(Error::FoldedRange(fold.start.utf16..fold.end.utf16));
        } // Same-Source finite anchors require no hidden-document traversal or parse.
        #[cfg(all(
            feature = "s2-table-window-prototype",
            feature = "paged-shaping-frontier"
        ))]
        if let Some(tables) = &self.payload.as_ref().ok_or(Error::Poisoned)?.tables
            && tables.contains(offset, true)?
        {
            // Table-visible records belong to their own actual domain; a raw
            // body ordinal cannot authorize boundaries internal to a table.
            return Err(Error::TableRange);
        }
        Ok((source.byte_len(), source.utf16_len()))
    }

    /// Use the explicit ordinal domain and original BreakJob records for non-BMP
    /// scalars and multi-scalar GCBs, without substituting UTF-16 unit counts.
    pub fn grapheme_ordinal_answer(
        &mut self,
        offset: usize,
    ) -> Result<MarkdownComposedGraphemeOrdinalAnswer> {
        let result = (|| {
            let (bytes, units) = self.grapheme_dimensions(offset)?;
            if offset > self.graphemes.covered.utf16 {
                // A sealed prefix cannot answer an untransferred suffix query.
                return Ok(MarkdownComposedGraphemeOrdinalAnswer::Pending);
            }
            let (ordinal, value) =
                graphemes::lower_bound(self.graphemes, offset, bytes, units, |record| {
                    self.row(record)
                })?;
            Ok(MarkdownComposedGraphemeOrdinalAnswer::Known(
                value.filter(|value| value.utf16 == offset).map(|_| ordinal),
            ))
        })();
        self.finish(result)
    }

    /// Adapt the established prepared-position query to its typed Pending error.
    /// Missing prefix records are recoverable and do not revoke the reader;
    /// real Source/FD/cancellation failures still pass through the same finish gate.
    pub fn grapheme_ordinal(&mut self, offset: usize) -> Result<Option<usize>> {
        match self.grapheme_ordinal_answer(offset)? {
            MarkdownComposedGraphemeOrdinalAnswer::Known(value) => Ok(value),
            MarkdownComposedGraphemeOrdinalAnswer::Pending => Err(Error::GraphemePrefixPending),
        }
    }

    /// Query an actual previous/next/exact boundary under this directory's
    /// domain and prefix coverage, retaining explicit Pending at an unseen tail.
    pub fn grapheme_query(
        &mut self,
        offset: usize,
        query: crate::MarkdownGraphemeQuery,
    ) -> Result<crate::MarkdownGraphemeAnswer> {
        use crate::{MarkdownGraphemeAnswer as Answer, MarkdownGraphemeQuery as Query};
        let result = (|| {
            let (bytes, units) = self.grapheme_dimensions(offset)?;
            if offset > self.graphemes.covered.utf16 {
                // Do not manufacture an end boundary from unavailable records.
                return Ok(Answer::Pending);
            }
            let (ordinal, value) =
                graphemes::lower_bound(self.graphemes, offset, bytes, units, |record| {
                    self.row(record)
                })?;
            let index = match query {
                Query::IsBoundary => {
                    // A lower-bound result is exact only at the requested UTF-16 coordinate.
                    return Ok(Answer::Known(value.filter(|value| value.utf16 == offset)));
                }
                Query::Previous => ordinal.checked_sub(1),
                Query::Next => {
                    // Advance past an exact boundary; between boundaries, the
                    // lower-bound record is already the requested successor.
                    let next = if value.is_some_and(|value| value.utf16 == offset) {
                        ordinal.checked_add(1).ok_or(Error::Overflow)?
                    } else {
                        ordinal
                    };
                    if next == self.graphemes.count && !self.graphemes.complete {
                        return Ok(Answer::Pending);
                    } // The covered final boundary is not Source EOF while transfer is incomplete.
                    Some(next)
                }
            };
            let value = match index {
                None => None, // There is no predecessor before actual start-of-text.
                Some(index) => {
                    graphemes::ordinal_boundary(self.graphemes, index, bytes, units, |record| {
                        self.row(record)
                    })?
                }
            };
            Ok(Answer::Known(value))
        })();
        self.finish(result)
    }
}
