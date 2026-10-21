//! Visual markers for actual Origin prefixes. Read only the authorized prefix; body spelling or a
//! bare coordinate cannot identify a container.

use super::*;
use pulldown_cmark::global_frontier::VerifiedInlinePrefix;

#[derive(Clone, Copy)]
enum Marker {
    Quote,
    Item,
    OrderedDigits,
}

/// Retain one actual prefix's current marker and physical-line activity, without a line directory,
/// complete string, or duplicate scope.
pub(in super::super::super) struct PrefixDecoration {
    // The same block's scanner issued this ticket; its range bounds every byte this cursor can
    // borrow. It retains no strong document/mmap owner and drops with the unique job on failure.
    proof: PrefixSource,
    next: usize,
    // None requires the original SourceRequest's same-Rc line, never a default false that hides the
    // active input line. Some records only this full selection's actual physical/EOL calculation;
    // it does not grant reusable authority for another generation.
    owner_active: Option<bool>,
    // A marker may cross small frames; retain fixed endpoints rather than a token list per nesting level.
    marker: Option<(Range<usize>, Marker)>,
}

/// Every variant comes from the original scanner/context. Leaf keeps its actual Origin; a bare
/// prefix range alone cannot grant presentation authority.
enum PrefixSource {
    Inline(VerifiedInlinePrefix),
    Leaf(BlockOriginLine),
    Paragraph(pulldown_cmark::global_frontier::InlineParagraphLine),
}

impl PrefixSource {
    fn range(&self) -> Range<usize> {
        match self {
            Self::Inline(proof) => proof.source_range(),
            Self::Leaf(origin) => origin.prefix_range(),
            Self::Paragraph(line) => line.prefix_range(),
        }
    }
}

impl PrefixDecoration {
    pub(super) fn new(proof: VerifiedInlinePrefix) -> Self {
        Self {
            next: proof.source_range().start,
            proof: PrefixSource::Inline(proof),
            owner_active: None,
            marker: None,
        }
    }

    /// Use the actual Leaf Origin's physical line/EOL for activity without rescanning its body.
    pub(in super::super::super) fn for_leaf(
        origin: BlockOriginLine,
        selection_bytes: &Range<usize>,
    ) -> Self {
        let active = crate::markdown_viewport_presentation::markdown_physical_line_is_active(
            selection_bytes,
            &origin.physical_range(),
            origin.eol_range().is_empty(),
        );
        Self {
            next: origin.prefix_range().start,
            proof: PrefixSource::Leaf(origin),
            owner_active: Some(active),
            marker: None,
        }
    }

    /// Borrow the physical-prefix fact already proved by the original finite paragraph context,
    /// without reading or retaining the complete line body.
    pub(in super::super::super) fn for_paragraph(
        line: pulldown_cmark::global_frontier::InlineParagraphLine,
        selection_bytes: &Range<usize>,
    ) -> Self {
        let active = crate::markdown_viewport_presentation::markdown_physical_line_is_active(
            selection_bytes,
            &line.physical_range(),
            line.eol_range().is_empty(),
        );
        Self {
            next: line.prefix_range().start,
            proof: PrefixSource::Paragraph(line),
            owner_active: Some(active),
            marker: None,
        }
    }

    /// Segment an Origin-proven structural prefix using the original quote/item marker widths.
    /// Extra indentation, residual tab columns, and paragraph whitespace retain body styling rather
    /// than becoming hidden simply because they lie in the prefix.
    pub(in super::super::super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        budget: usize,
        footnote_openings: &[Range<usize>],
    ) -> Result<(SemanticWork, Option<SpanProof>, bool), MarkdownTableRequestError> {
        let active = self
            .owner_active
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        let mut work = SemanticWork {
            structural_transitions: PLAIN_CONTROL,
            ..Default::default()
        };
        let end = self.proof.range().end;
        // Finish a trailing marker at the proven prefix end. That finite endpoint is not source EOF;
        // the outer request checks Source health, and this cursor creates no synthetic EOF frame.
        if self.next == end {
            let marker = self
                .marker
                .take()
                .map(|(range, kind)| Self::span(range, kind, active));
            return Ok((work, marker, true));
        }
        // The producer ordered nonoverlapping opening facts. Locate only the next opening for this
        // prefix and charge actual comparisons, instead of rescanning all ancestors per byte/line.
        let mut comparisons = 0;
        let index = footnote_openings.partition_point(|opening| {
            comparisons += 1;
            opening.end <= self.next
        });
        work.structural_transitions += comparisons;
        let end = match footnote_openings.get(index) {
            Some(opening) if opening.start <= self.next => {
                if let Some((range, kind)) = self.marker.take() {
                    return Ok((work, Some(Self::span(range, kind, active)), false));
                } // Return the real preceding quote/item before padding can cross into the label.
                self.next = opening.end.min(end);
                return Ok((work, None, self.next == end));
            }
            Some(opening) => end.min(opening.start),
            None => end,
        }; // Preserve literal labels/colons/consumed whitespace; only the original scanner authorizes this skip.
        // Charge the actual tree path and prefix work without scanning a long physical body for its
        // owner. Insufficient budget fails explicitly instead of completing partial digits/padding.
        let tree = tree_height(document);
        let wanted =
            (budget.saturating_sub(work.structural_transitions + tree) / 4).min(end - self.next);
        if wanted == 0 {
            return Err(MarkdownTableRequestError::WorkLimit);
        }
        let frame = document
            .table_source_frame(self.next, wanted)
            .map_err(MarkdownTableRequestError::Source)?;
        work.source_tree_steps = tree;
        work.structural_transitions += tree;
        let mut found = None;
        let mut mismatch = false;
        for &byte in frame.bytes.iter().take(end - self.next) {
            // Charge one actual peek and at most two transitions. Unconsumed padding lookahead is
            // read again in the next quantum.
            work.source_bytes += 1;
            work.structural_transitions += 2;
            if let Some((range, kind)) = self.marker.as_mut() {
                match kind {
                    Marker::OrderedDigits if byte.is_ascii_digit() => {
                        range.end += 1;
                        self.next += 1;
                        continue;
                    }
                    Marker::OrderedDigits if matches!(byte, b'.' | b')') => {
                        range.end += 1;
                        self.next += 1;
                        *kind = Marker::Item;
                        continue;
                    }
                    Marker::OrderedDigits => {
                        // An ordered prefix without its actual delimiter contradicts the scanner ticket.
                        mismatch = true;
                        break;
                    }
                    Marker::Quote | Marker::Item => {
                        let padding = byte == b' ' || matches!(kind, Marker::Item) && byte == b'\t';
                        if padding {
                            range.end += 1;
                            self.next += 1;
                        } // A quote includes only one following ASCII space; a tab stays outside its hidden bytes.
                        let (range, kind) = self.marker.take().unwrap();
                        found = Some(Self::span(range, kind, active));
                        break;
                    }
                }
            }
            // The original scope/scanner already proved the container prefix and excluded footnote
            // openings. Spelling determines only its visual width; identical body characters have
            // no authority to enter this path.
            let at = self.next;
            self.next += 1;
            let kind = match byte {
                b'>' => Some(Marker::Quote),
                b'-' | b'+' | b'*' => Some(Marker::Item),
                b'0'..=b'9' => Some(Marker::OrderedDigits),
                _ => None,
            };
            self.marker = kind.map(|kind| (at..self.next, kind));
        }
        // Return the real lease before consuming visual results, preserving reclaim failure over a
        // found marker. A ticket mismatch fails this request without editing canonical text/history.
        frame.finish().map_err(MarkdownTableRequestError::Source)?;
        if mismatch {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        Ok((work, found, false))
    }

    fn span(range: Range<usize>, kind: Marker, active: bool) -> SpanProof {
        let mut styles = MarkdownPresentationStyles::MARKER;
        if matches!(kind, Marker::Quote) && !active {
            styles = styles.union(MarkdownPresentationStyles::HIDDEN_MARKER);
        }
        // Equal body/whole ranges preserve visibility already decided by the physical-line owner;
        // the ordinary inline owner must not decide it a second time.
        SpanProof {
            whole: range.clone(),
            body: range,
            styles,
        }
    }
}

impl CodeJob {
    /// Resume each prefix from the last consumed original record, without another source directory
    /// for long tables or multiline windows.
    pub(in super::super) fn prefix_origin_record_index(&self) -> Option<usize> {
        self.prefix
            .as_ref()
            .filter(|prefix| prefix.owner_active.is_none())
            .map(|_| self.prefix_record_index)
    }

    /// After the parent checks same-Rc block identity, verify the exact prefix and consume only this
    /// original record's finite facts.
    pub(in super::super) fn advance_prefix_origin(
        &mut self,
        origin: Option<&BlockOriginLine>,
        selection_bytes: &Range<usize>,
    ) -> Result<SemanticWork, MarkdownTableRequestError> {
        // The parent retains the original page. Borrow one record without retaining its Rc or line
        // in another owner graph; numeric resemblance cannot replace the prior same-block check.
        let prefix = self
            .prefix
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)?;
        if prefix.owner_active.is_some() {
            return Err(MarkdownTableRequestError::InvalidRequest);
        }
        if let Some(origin) = origin {
            let PrefixSource::Inline(proof) = &prefix.proof else {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }; // Only the original Inline prefix can be paired here; a similarly positioned Leaf grants no authority.
            let target = proof.source_range().start;
            let physical = origin.physical_range();
            if physical.start > target {
                return Err(MarkdownTableRequestError::InvalidRequest);
            }
            if physical.start == target {
                if !proof.matches_origin(origin) {
                    return Err(MarkdownTableRequestError::InvalidRequest);
                }
                // CRLF's EOL endpoint belongs to the next line; only a true final line without EOL
                // accepts an EOF caret. The shared preview predicate uses the two actual selection
                // endpoints, including reverse selections, rather than activating every middle line.
                prefix.owner_active = Some(
                    crate::markdown_viewport_presentation::markdown_physical_line_is_active(
                        selection_bytes,
                        &physical,
                        origin.eol_range().is_empty(),
                    ),
                );
            }
        }
        // Adjacent prefixes borrow forward from this SourceRequest's ordered page, without resetting
        // the consumed-record index or cumulative budget.
        self.prefix_record_index += 1;
        Ok(SemanticWork {
            structural_transitions: PLAIN_CONTROL + std::mem::size_of::<Rc<SourceRecord>>(),
            ..Default::default()
        })
    }
}
