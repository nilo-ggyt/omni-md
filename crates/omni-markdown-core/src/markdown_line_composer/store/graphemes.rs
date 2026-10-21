//! Migrate same-Source graphemes into the unique output FD for the actual layout
//! prefix. Explicit visible ordinal domains do not impersonate raw whole-Source
//! ranks inside unparsed hidden ranges.
use super::*;
use crate::{MarkdownGraphemeBoundary, MarkdownLayoutBreakJob};

mod links;
mod search;
use links::Ancestors;
pub(super) use search::BoundarySearch;
pub(super) use search::{lower_bound, ordinal_boundary};

pub(super) const GRAPHEME: u8 = 12;

/// Fixed directory for noncontiguous records. Count belongs to its explicit domain;
/// filtered TableVisible/FoldedVisible ranks are not raw whole-Source ordinals.
/// Covered tracks migrated grapheme metadata, not glyph-layout coverage.
#[derive(Clone, Copy)]
pub(super) struct GraphemeDirectory {
    pub domain: crate::MarkdownGraphemeOrdinalDomain,
    pub last: usize,
    pub count: usize,
    pub total: Option<usize>,
    pub covered: MarkdownGraphemeBoundary,
    pub complete: bool,
}
impl GraphemeDirectory {
    pub(super) fn validate(&self, bytes: usize, utf16: usize, records: usize) -> Result<()> {
        // A complete directory needs both its exact domain count and actual
        // byte/UTF-16 EOF. Neither known Source length nor a count alone grants EOF.
        let eof = self.covered == (MarkdownGraphemeBoundary { byte: bytes, utf16 });
        if self.count == 0
            || self.total.is_some_and(|total| self.count > total)
            || self.last >= records
            || self.covered.byte > bytes
            || self.covered.utf16 > utf16
            || self.complete != (self.total == Some(self.count))
            || (self.complete && !eof)
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(())
    }
}

pub(super) struct GraphemeCapture {
    domain: crate::MarkdownGraphemeOrdinalDomain,
    total: Option<usize>,
    written: usize,
    read_ordinal: usize,
    source_total: Option<usize>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    skip_search: Option<(usize, usize)>,
    last: Option<(usize, MarkdownGraphemeBoundary)>,
    pending: Option<MarkdownGraphemeBoundary>,
    ancestors: Ancestors,
}

impl GraphemeCapture {
    /// A real UBA seam may precede the next grapheme. Only a buffered boundary
    /// required by this item still prevents glyph capture.
    pub(super) fn has_buffered_through(&self, through: usize) -> bool {
        self.pending
            .is_some_and(|boundary| boundary.utf16 <= through)
    }

    fn directory(&self) -> Result<GraphemeDirectory> {
        let (last, covered) = self.last.ok_or(Error::InvalidRequest)?;
        Ok(GraphemeDirectory {
            domain: self.domain,
            last,
            covered,
            count: self.written,
            total: self.total,
            complete: self.total == Some(self.written),
        })
    }
}

/// Records count actual 64-byte writes; metadata_units count original grapheme
/// reads. Neither allowance borrows from the other.
pub(in crate::markdown_line_composer) struct GraphemeCaptureStep {
    pub records: usize,
    pub metadata_units: usize,
    pub ready: bool,
}

/// Ordinary controls still require actual grapheme endpoints. An internal shaping
/// endpoint needs the original ItemScan's same-Source seam proof.
#[derive(Clone, Copy)]
enum CaptureEnd<'a> {
    Grapheme(usize),
    BidiSeam(&'a crate::markdown_line_composer::item::ResolvedBidiShapingSeam),
}

impl CaptureEnd<'_> {
    fn utf16(self, authority: &MarkdownLayoutTextAuthority) -> Result<usize> {
        match self {
            Self::Grapheme(end) => Ok(end),
            Self::BidiSeam(proof) => Ok(proof.boundary(authority)?.1),
        }
    }
}

impl GlyphStore {
    /// Migrate boundaries only through the actual original item/control endpoint.
    /// Read no text; budget edges are neither grapheme boundaries nor EOF.
    pub(in crate::markdown_line_composer) fn capture_graphemes_through(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        through_utf16: usize,
        records: usize,
        metadata_units: usize,
    ) -> Result<GraphemeCaptureStep> {
        self.capture_graphemes_until(
            breaks,
            CaptureEnd::Grapheme(through_utf16),
            records,
            metadata_units,
        )
    }

    /// An item's internal UBA seam is not a grapheme record. Keep the real next
    /// boundary buffered for migration after its following item is ready.
    pub(in crate::markdown_line_composer) fn capture_item_graphemes(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        item: &MarkdownLineItem,
        records: usize,
        metadata_units: usize,
    ) -> Result<GraphemeCaptureStep> {
        item_shaping_seams(item, &self.authority)?;
        let target = match &item.shaping_seams()[1] {
            Some(proof) => CaptureEnd::BidiSeam(proof),
            None => CaptureEnd::Grapheme(item.facts().source_utf16.end),
        };
        self.capture_graphemes_until(breaks, target, records, metadata_units)
    }

    fn capture_graphemes_until(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        target: CaptureEnd<'_>,
        records: usize,
        metadata_units: usize,
    ) -> Result<GraphemeCaptureStep> {
        let result = (|| {
            self.guard()?;
            let through_utf16 = target.utf16(&self.authority)?;
            if records == 0
                || metadata_units == 0
                || records > STEP
                || metadata_units > STEP
                || through_utf16 > self.source.utf16_len()
                || self.capture.is_some()
                || self.control_capture.is_some()
                || !matches!(self.index, Some(Rows::Writing(_)))
            {
                return Err(Error::InvalidRequest);
            }
            let mut step = GraphemeCaptureStep {
                records: 0,
                metadata_units: 0,
                ready: false,
            };
            // Preserve the original Break Job's explicit ordinal domain and stable
            // committed prefix. Frontier mode may still have an unknown total.
            let domain = breaks.grapheme_ordinal_domain(&self.authority)?;
            #[cfg(feature = "paged-shaping-frontier")]
            let (known, total) = {
                let prefix = breaks.grapheme_prefix_facts(&self.authority)?;
                (prefix.known_boundary_count, prefix.total_count)
            };
            #[cfg(not(feature = "paged-shaping-frontier"))]
            let (known, total) = {
                let Some(total) = breaks.ready_grapheme_count(&self.authority)? else {
                    return Ok(step);
                };
                (total, Some(total))
            };
            if known == 0 {
                // No original first record exists yet. A bounded query leaves real
                // dependency demand; an empty prepared prefix cannot mean EOF.
                #[cfg(feature = "paged-shaping-frontier")]
                {
                    step.metadata_units = 1;
                    breaks.read_grapheme_prefix(
                        &self.authority,
                        0..1,
                        &mut [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }],
                    )?;
                }
                return Ok(step);
            }
            let capture = self.graphemes.get_or_insert_with(|| GraphemeCapture {
                domain,
                total: if domain == crate::MarkdownGraphemeOrdinalDomain::TableVisible {
                    None
                } else {
                    total
                },
                written: 0,
                read_ordinal: 0,
                source_total: total,
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                skip_search: None,
                last: None,
                pending: None,
                ancestors: Ancestors::new(),
            });
            // Once known, the original raw total/domain cannot change beneath this
            // capture. Table filtering derives its own final visible total later.
            if capture.domain != domain
                || capture.source_total.is_some() && total != capture.source_total
            {
                return Err(Error::InconsistentCoverage);
            }
            capture.source_total = total;
            if domain != crate::MarkdownGraphemeOrdinalDomain::TableVisible {
                capture.total = total;
            }
            // Seal a visible-domain total only after filtering/writing actual EOF;
            // the original raw boundary count is not the visible count.
            let file = self.index.as_mut().ok_or(Error::Poisoned)?;
            loop {
                if let Some((_, covered)) = capture.last
                    && covered.utf16 >= through_utf16
                {
                    if covered.utf16 != through_utf16 && step.records != 0 {
                        return Err(Error::InconsistentCoverage);
                    } // New writes must land on a real item grapheme; an already
                    // migrated longer prefix may serve an earlier shorter line.
                    step.ready = true;
                    break;
                }
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                if let Some((mut low, mut high)) = capture.skip_search {
                    // A table span may overlap raw metadata already prepared by the
                    // original Job. Find its real end inside that known prefix
                    // under the same metadata quota, without copying its interior.
                    if low < high {
                        if step.metadata_units == metadata_units {
                            break;
                        }
                        let middle = low + (high - low) / 2;
                        let mut value = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                        step.metadata_units += 1;
                        if !breaks.read_grapheme_prefix(
                            &self.authority,
                            middle..middle + 1,
                            &mut value,
                        )? {
                            break;
                        }
                        let span = self.table_skip.ok_or(Error::InconsistentCoverage)?;
                        if value[0].utf16 < span.end.utf16 {
                            low = middle + 1;
                        } else {
                            high = middle;
                        }
                        capture.skip_search = Some((low, high));
                        continue;
                    }
                    capture.read_ordinal = low;
                    capture.skip_search = None;
                }
                if capture.pending.is_none() {
                    if step.metadata_units == metadata_units {
                        break;
                    }
                    if total == Some(capture.read_ordinal) {
                        return Err(Error::InconsistentCoverage);
                    }
                    let mut value = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }];
                    let end = capture.read_ordinal.checked_add(1).ok_or(Error::Overflow)?;
                    step.metadata_units += 1;
                    #[cfg(feature = "paged-shaping-frontier")]
                    let read = breaks.read_grapheme_prefix(
                        &self.authority,
                        capture.read_ordinal..end,
                        &mut value,
                    )?;
                    #[cfg(not(feature = "paged-shaping-frontier"))]
                    let read = breaks.read_ready_graphemes(
                        &self.authority,
                        capture.read_ordinal..end,
                        &mut value,
                    )?;
                    if !read {
                        break;
                    } // Retain this capture while a later quantum advances the same GCB Job.
                    capture.read_ordinal = end;
                    capture.pending = Some(value[0]); // Retain one read record across write-budget yields.
                }
                let value = capture.pending.ok_or(Error::InconsistentCoverage)?;
                if value.utf16 > through_utf16 && matches!(target, CaptureEnd::BidiSeam(_)) {
                    // The original Break Job returned the real next grapheme after
                    // this item's UBA seam. Write no fabricated boundary and keep
                    // that actual record for the one following item to consume.
                    step.ready = true;
                    break;
                }
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                if self.table_skip.is_some_and(|span| {
                    span.start.utf16 < value.utf16 && value.utf16 < span.end.utf16
                }) {
                    // The original Job may have seen old short rows. Search only
                    // its actual known prefix; do not migrate internal raw boundaries.
                    capture.pending = None;
                    capture.skip_search = Some((capture.read_ordinal, known));
                    continue;
                }
                if step.records == records {
                    break;
                }
                if value.byte > self.source.byte_len()
                    || value.utf16 > self.source.utf16_len()
                    || capture.last.is_some_and(|(_, previous)| {
                        previous.byte >= value.byte || previous.utf16 >= value.utf16
                    })
                    || (capture.written == 0
                        && value != (MarkdownGraphemeBoundary { byte: 0, utf16: 0 }))
                {
                    return Err(Error::InconsistentCoverage);
                }
                // Append and publish this same actual boundary in strict Source
                // order. Update skip ancestors only after the file append succeeds.
                let record = file.len();
                let row = capture.ancestors.encode(
                    value,
                    capture.written,
                    capture.last.map(|value| value.0),
                )?;
                file.append(row)?;
                capture.ancestors.publish(capture.written, record)?;
                capture.written = capture.written.checked_add(1).ok_or(Error::Overflow)?;
                capture.last = Some((record, value));
                capture.pending = None;
                step.records += 1;
                if total == Some(capture.read_ordinal)
                    && value
                        != (MarkdownGraphemeBoundary {
                            byte: self.source.byte_len(),
                            utf16: self.source.utf16_len(),
                        })
                {
                    return Err(Error::InconsistentCoverage);
                } // Actual raw count and byte/UTF-16 end must agree before claiming EOF.
                if total == Some(capture.read_ordinal) {
                    capture.total = Some(capture.written);
                }
            }
            breaks.require_source(&self.authority)?;
            Ok(step)
        })();
        self.finish(result)
    }

    /// Finite tests may migrate the complete real table as an oracle. Production
    /// entries pass only actual item/control endpoints.
    #[cfg(test)]
    pub(in crate::markdown_line_composer) fn capture_graphemes(
        &mut self,
        breaks: &mut MarkdownLayoutBreakJob,
        records: usize,
    ) -> Result<bool> {
        Ok(self
            .capture_graphemes_through(
                breaks,
                self.source.utf16_len(),
                records.min(STEP),
                records.min(STEP),
            )?
            .ready)
    }

    pub(super) fn grapheme_directory(&self) -> Result<GraphemeDirectory> {
        // Validate this original writer's current metadata facts against actual
        // Source dimensions and appended record bounds before any query uses them.
        self.guard()?;
        let directory = self
            .graphemes
            .as_ref()
            .ok_or(Error::InvalidRequest)?
            .directory()?;
        directory.validate(
            self.source.byte_len(),
            self.source.utf16_len(),
            self.index.as_ref().ok_or(Error::Poisoned)?.len(),
        )?;
        Ok(directory)
    }

    pub(super) fn published_grapheme_directory(&self) -> Result<GraphemeDirectory> {
        self.guard()?;
        self.published_graphemes.ok_or(Error::InvalidRequest)
    }

    pub(super) fn is_grapheme(&mut self, offset: usize) -> Result<bool> {
        let directory = self.grapheme_directory()?;
        if offset > self.source.utf16_len() {
            return Err(Error::InvalidRequest);
        }
        if offset > directory.covered.utf16 {
            // Missing migrated records remain Pending, never known non-boundaries.
            return Err(Error::GraphemePrefixPending);
        }
        let bytes = self.source.byte_len();
        let units = self.source.utf16_len();
        let (_, value) = lower_bound(directory, offset, bytes, units, |record| {
            self.guard()?;
            Ok(self.index.as_mut().ok_or(Error::Poisoned)?.read(record)?)
        })?;
        self.guard()?;
        Ok(value.is_some_and(|value| value.utf16 == offset))
    }

    /// Check both exact endpoints and return this query's decoded right cursor.
    /// A non-grapheme returns None, never a replacement proof. Whole lines start
    /// from the original directory; internal parts borrow a line-local right-cursor
    /// copy to search left without Store state or prefetch.
    pub(super) fn grapheme_range_end(
        &mut self,
        range: &Range<usize>,
        preceding: Option<&BoundarySearch>,
    ) -> Result<Option<BoundarySearch>> {
        let directory = self.grapheme_directory()?;
        if range.start > range.end || range.end > self.source.utf16_len() {
            return Err(Error::InvalidRequest);
        }
        // Output covers actual committed graphemes only. A farther endpoint waits
        // for original progress instead of being classified as a grapheme interior.
        if range.end > directory.covered.utf16 {
            return Err(Error::GraphemePrefixPending);
        }
        let bytes = self.source.byte_len();
        let units = self.source.utf16_len();
        let mut query = match preceding {
            Some(right) => {
                // This line's actual right endpoint was decoded already. Copy only
                // fixed query facts; subsequent step reads retain ordinal, skip,
                // predecessor, and Source-order authentication.
                let mut query = right.clone();
                query.preceding_offset(range.end)?;
                query
            }
            None => BoundarySearch::offset(range.end),
        };
        // Locate right first, then search left through the same authenticated reverse
        // links. Both dimensions still come from the original frozen Source.
        let right = query
            .step(directory, bytes, units, usize::MAX, |record| {
                self.guard()?;
                Ok(self.index.as_mut().ok_or(Error::Poisoned)?.read(record)?)
            })?
            .answer
            .ok_or(Error::InconsistentCoverage)?
            .1;
        self.guard()?; // Recheck original Source/cancellation after actual I/O.
        // The left query moves its cursor; retain right only for other parts in this
        // same line assembly. Even a decoded-current hit retains both health gates.
        let right_query = query.clone();
        query.preceding_offset(range.start)?;
        let left = query
            .step(directory, bytes, units, usize::MAX, |record| {
                self.guard()?;
                Ok(self.index.as_mut().ok_or(Error::Poisoned)?.read(record)?)
            })?
            .answer
            .ok_or(Error::InconsistentCoverage)?
            .1;
        self.guard()?;
        // Lower-bound search may return a real boundary to the target's right.
        // Both endpoints must match exactly before this complete range is accepted.
        Ok((left.is_some_and(|value| value.utf16 == range.start)
            && right.is_some_and(|value| value.utf16 == range.end))
        .then_some(right_query))
    }
}
