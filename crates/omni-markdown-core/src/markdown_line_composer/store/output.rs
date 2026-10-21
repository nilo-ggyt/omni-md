//! Read-only delivery after an actual LineEnd. Line/Part values retain weak
//! original-output tickets and expose no public constructor from a raw range.
//!
//! Coordinates use 26.6 units relative to this line's baseline. The platform
//! derives actual ascent/descent/leading and global Y from the same registered
//! font; this reader grants no fixed line height or Native viewport readiness.

use super::*;
use crate::MarkdownLayoutTextSourceObserver;

mod grapheme_queries;
#[cfg(all(
    feature = "s2-table-window-prototype",
    feature = "paged-shaping-frontier"
))]
mod table_row;
pub use grapheme_queries::MarkdownComposedGraphemeOrdinalAnswer;
mod visual_caret;
pub use visual_caret::{
    MarkdownComposedCaretAssociation, MarkdownComposedCaretSide, MarkdownComposedVisualAnswer,
    MarkdownComposedVisualGraphemeAnswer, MarkdownComposedVisualGraphemeQuery,
    MarkdownComposedVisualGraphemeStep, MarkdownComposedVisualQuery, MarkdownComposedVisualStep,
};
#[cfg(feature = "paged-shaping-frontier")]
pub use visual_caret::{
    MarkdownComposedVisualWordAnswer, MarkdownComposedVisualWordQuery,
    MarkdownComposedVisualWordStep,
};
mod lookup;
mod trailing_whitespace;
pub use lookup::{
    MarkdownComposedLineAffinity, MarkdownComposedLineLookup, MarkdownComposedLineLookupAnswer,
    MarkdownComposedLineLookupStep, MarkdownComposedLineLookupTarget,
};
pub use trailing_whitespace::{
    MarkdownComposedLineTrailingWhitespace, MarkdownComposedLineTrailingWhitespaceStep,
};

pub(super) const LINE: u8 = 8;
pub(super) const PART: u8 = 9;
pub(super) const PART_META: u8 = 10;
pub(super) const LINE_END: u8 = 11;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownComposedLineKind {
    Allowed,
    Mandatory,
    Emergency,
    Overflow,
    /// An original canonical structural row; the actual Grid consumer determines
    /// its final laid-out height under the same row authority.
    TableRow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownComposedLineFacts {
    pub source_utf16: Range<usize>,
    pub paragraph_utf16: Range<usize>,
    pub parts: usize,
    pub glyphs: usize,
    pub ordinal: usize,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
    pub kind: MarkdownComposedLineKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarkdownComposedLinePartFacts {
    /// Left/right inset already sealed by the same font response; no extra
    /// directory record or live-document style lookup is introduced.
    pub paragraph_inset_26_6: i32,
    /// Sealed by this part's original item, never inferred later by the platform
    /// from an arbitrary character range.
    pub presentation: crate::MarkdownPresentationStyles,
    pub kind: super::super::MarkdownComposedLinePartKind,
    pub source_utf16: Range<usize>,
    pub direction: MarkdownShapingDirection,
    pub level: u8,
    pub script: [u8; 4],
    pub points: f32,
    pub origin_x_26_6: i64,
    pub origin_y_26_6: i64,
    pub x_advance_26_6: i64,
    pub y_advance_26_6: i64,
    pub glyphs: usize,
    pub visual_glyphs: Range<usize>,
}

/// Obtain only from a complete authenticated line directory. Weak output
/// identity prevents copied facts from issuing parts for another output.
pub struct MarkdownComposedLine {
    output: Weak<()>,
    header: usize,
    end: usize,
    previous: Option<usize>,
    skip: Option<(usize, usize)>,
    base_level: u8,
    facts: MarkdownComposedLineFacts,
}
impl MarkdownComposedLine {
    /// Borrow established Copy/range facts without acquiring new reader authority.
    pub fn facts(&self) -> &MarkdownComposedLineFacts {
        &self.facts
    }
    /// Read the base level already authenticated by LineLookup, without new directory or lifetime authority.
    pub fn base_level(&self) -> u8 {
        self.base_level
    }
}

/// Retain neither Reader, Source, nor Font. The platform must keep the original
/// output and actual font-registration owner alive while using this part.
pub struct MarkdownComposedLinePart {
    output: Weak<()>,
    source: MarkdownLayoutTextAuthority,
    registration: MarkdownLineFontRegistration,
    glyph_start: usize,
    cluster_start: usize,
    cluster_count: usize,
    facts: MarkdownComposedLinePartFacts,
}
impl MarkdownComposedLinePart {
    /// Expose sealed metadata only; these facts alone cannot authorize drawing.
    pub fn facts(&self) -> &MarkdownComposedLinePartFacts {
        &self.facts
    }
    /// Preserve the original Source ticket for checks at the actual use boundary.
    pub fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.source
    }
    /// Return the weak original registration rather than retaining a font graph.
    pub fn font_registration(&self) -> &MarkdownLineFontRegistration {
        &self.registration
    }
}

/// Own the single actual reader and the exact Source/font/fold/table claims
/// needed to authenticate its sealed metadata. Revocation drops this group.
struct Payload {
    reader: LayoutIndexReader<64>,
    source: MarkdownLayoutTextSource,
    fonts: MarkdownLineFontCatalog,
    folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
}

/// Own one read-only output FD without complete glyph/item/line/font or document
/// vectors. Queries read bounded records through this same owner.
pub struct MarkdownComposedLines {
    payload: Option<Payload>,
    identity: Arc<()>,
    // Lineage authenticates append-only prefixes from the same writer. Line/Part
    // identities still cannot cross snapshots merely because lineage matches.
    #[cfg(feature = "paged-shaping-frontier")]
    lineage: Weak<()>,
    authority: MarkdownLayoutTextAuthority,
    cancellation: MarkdownLineComposeCancellation,
    last: Option<usize>,
    count: usize,
    range: Range<usize>,
    graphemes: GraphemeDirectory,
    observer: MarkdownComposedLineResourceObserver,
}

/// Observe actual file/mapping and same-Source release facts. Expired Line/Part
/// tickets alone cannot establish that underlying resources really returned.
#[derive(Clone)]
pub struct MarkdownComposedLineResourceObserver {
    file: LayoutIndexObserver,
    source: MarkdownLayoutTextSourceObserver,
    backing: Option<crate::MarkdownOriginalBackingObserver>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownComposedLineResources {
    pub file_open: bool,
    pub mapped_bytes: usize,
    pub peak_mapped_bytes: usize,
    /// The sole metadata write buffer; the Reader retains only its original File
    /// after this buffer has actually returned.
    pub buffer_bytes: usize,
    pub peak_buffer_bytes: usize,
    pub cleanup_failed: bool,
    pub reader_failed: bool,
    pub source_payload_released: bool,
    pub first_source_errno: Option<i32>,
}
impl MarkdownComposedLineResourceObserver {
    /// Capture the actual owners' fixed receipts, without retaining Source
    /// payload or duplicating its file descriptor.
    pub(super) fn for_source(file: LayoutIndexObserver, source: &MarkdownLayoutTextSource) -> Self {
        Self {
            file,
            source: source.resource_observer(),
            backing: source.original_backing_observer(),
        }
    }
    /// Read resource facts independently of weak line/part revocation. These
    /// observations describe this owner rather than whole-product RSS.
    pub fn snapshot(&self) -> MarkdownComposedLineResources {
        let file = self.file.snapshot();
        MarkdownComposedLineResources {
            file_open: file.file_open,
            mapped_bytes: file.mapped_bytes,
            peak_mapped_bytes: file.peak_mapped_bytes,
            buffer_bytes: file.buffer_bytes,
            peak_buffer_bytes: file.peak_buffer_bytes,
            cleanup_failed: file.cleanup_failed,
            reader_failed: file.reader_failed,
            source_payload_released: self.source.payload_released(),
            first_source_errno: self.backing.as_ref().and_then(|value| value.first_errno()),
        }
    }
}

/// Only the producer's actual LineEnd flow creates this handoff. Its File and
/// directory coordinates are not exposed as caller-supplied output authority.
pub(super) struct OutputSeal {
    pub source: MarkdownLayoutTextSource,
    pub authority: MarkdownLayoutTextAuthority,
    pub fonts: MarkdownLineFontCatalog,
    pub cancellation: MarkdownLineComposeCancellation,
    pub reader: LayoutIndexReader<64>,
    pub identity: Arc<()>,
    // The original GlyphStore supplies lineage at actual LineEnd. A Word
    // continuation cannot extend authority across reset or a foreign writer.
    #[cfg(feature = "paged-shaping-frontier")]
    pub lineage: Weak<()>,
    pub last: Option<usize>,
    pub count: usize,
    pub range: Range<usize>,
    pub graphemes: GraphemeDirectory,
    pub folds: Option<crate::markdown_layout_source::folds::FoldPlan>,
    #[cfg(all(
        feature = "s2-table-window-prototype",
        feature = "paged-shaping-frontier"
    ))]
    pub tables: Option<crate::markdown_layout_source::table_rows::TablePlan>,
}

impl MarkdownComposedLines {
    /// Validate this actual sealed prefix before taking over its single reader.
    /// Records prefetched after its last LineEnd remain outside its authority.
    pub(super) fn from_sealed(seal: OutputSeal) -> Result<Self> {
        seal.source.health()?;
        if !seal.source.permits(&seal.authority)
            || seal.range.start > seal.range.end
            || seal.range.end > seal.source.utf16_len()
            || (seal.count == 0) != seal.last.is_none()
        {
            // Source identity, range coverage, and the actual last-line marker
            // must agree before any read-only output can become public.
            return Err(Error::InconsistentCoverage);
        }
        if seal.cancellation.is_cancelled() {
            // A cancelled producer cannot transfer an apparently ready output.
            return Err(Error::Cancelled);
        }
        seal.graphemes.validate(
            seal.source.byte_len(),
            seal.source.utf16_len(),
            seal.reader.len(),
        )?;
        if seal.graphemes.covered.utf16 < seal.range.end
            || seal.last.is_some_and(|last| seal.graphemes.last >= last)
        {
            return Err(Error::InconsistentCoverage);
        } // Grapheme coverage and last record must belong to this exact LineEnd prefix.
        let observer = MarkdownComposedLineResourceObserver::for_source(
            seal.reader.resource_observer(),
            &seal.source,
        );
        let value = Self {
            payload: Some(Payload {
                reader: seal.reader,
                source: seal.source,
                fonts: seal.fonts,
                folds: seal.folds,
                #[cfg(all(
                    feature = "s2-table-window-prototype",
                    feature = "paged-shaping-frontier"
                ))]
                tables: seal.tables,
            }),
            identity: seal.identity,
            #[cfg(feature = "paged-shaping-frontier")]
            lineage: seal.lineage,
            authority: seal.authority,
            cancellation: seal.cancellation,
            last: seal.last,
            count: seal.count,
            range: seal.range,
            graphemes: seal.graphemes,
            observer,
        };
        // Recheck source/cancellation/reader health on the assembled original
        // owner before the candidate escapes this acquisition boundary.
        value.require(&value.authority)?;
        Ok(value)
    }
    /// Return the original immutable ticket, never a recaptured current source.
    pub fn source_authority(&self) -> &MarkdownLayoutTextAuthority {
        &self.authority
    }
    /// This finite sealed prefix need not cover the complete document.
    pub fn source_range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// Observing release does not keep this reader or document payload alive.
    pub fn resource_observer(&self) -> MarkdownComposedLineResourceObserver {
        self.observer.clone()
    }

    /// Perform Core-only checks. Native callers must check their own original
    /// current predicate before/after the outer RefCell borrow, never inside it.
    pub fn require(&self, authority: &MarkdownLayoutTextAuthority) -> Result<()> {
        if let Some(errno) = self
            .observer
            .backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            // Preserve the original backing failure before a later poisoned or
            // expired wrapper could replace its typed cause.
            return Err(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        }
        let payload = self.payload.as_ref().ok_or(Error::Poisoned)?;
        payload.source.health()?;
        if authority != &self.authority || !payload.source.permits(authority) {
            // Equal offsets or a newer ticket cannot authorize this old output.
            return Err(Error::SourceMismatch);
        }
        if self.cancellation.is_cancelled() {
            // Cancellation remains terminal even if every file is still open.
            return Err(Error::Cancelled);
        }
        let file = self.observer.file.snapshot();
        if !file.file_open
            || file.mapped_bytes != 0
            || file.buffer_bytes != 0
            || file.cleanup_failed
            || file.reader_failed
        {
            // A settled reader owns no active map/write buffer. Closed files or
            // recorded backend failures revoke it rather than manufacturing EOF.
            return Err(Error::Poisoned);
        }
        Ok(())
    }
    /// Complete one query and revoke the actual payload on terminal failure.
    /// Fold/table exclusions are recoverable only while original health holds.
    fn finish<T>(&mut self, result: Result<T>) -> Result<T> {
        if matches!(&result, Err(Error::FoldedRange(_) | Error::TableRange)) {
            // This ordinal domain legitimately omits hidden/internal GCBs.
            // Backend health failure still permanently returns the actual reader.
            if let Err(error) = self.require(&self.authority) {
                drop(self.payload.take());
                return Err(error);
            }
            return result;
        }
        let result = result.and_then(|value| {
            self.require(&self.authority)?;
            Ok(value)
        });
        if result.is_err() {
            drop(self.payload.take());
        } // Terminal failure drops the sole reader/Source; old lines cannot regain authority.
        if let Some(errno) = self
            .observer
            .backing
            .as_ref()
            .and_then(|value| value.first_errno())
        {
            return Err(
                crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(errno).into(),
            );
        }
        result
    }
    /// Read one fixed record only after the same output's actual health check.
    fn row(&mut self, record: usize) -> Result<[u8; 64]> {
        self.require(&self.authority)?;
        Ok(self
            .payload
            .as_mut()
            .ok_or(Error::Poisoned)?
            .reader
            .read(record)?)
    }
    /// Authenticate LineEnd shape and checked part extent before following its
    /// header pointer. A malformed directory cannot redirect record reads.
    fn line_header(&self, end: usize, tail: &[u8; 64]) -> Result<usize> {
        if tail[56] != LINE_END || tail[57..].iter().any(|byte| *byte != 0) {
            return Err(Error::InconsistentCoverage);
        }
        let header = usize_at(tail, 0)?;
        let parts = usize_at(tail, 8)?;
        if parts == 0
            || header.checked_add(1).and_then(|value| {
                parts
                    .checked_mul(2)
                    .and_then(|parts| value.checked_add(parts))
            }) != Some(end)
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(header)
    }
    /// Read the original tail/header pair and decode without another directory.
    fn line_at(&mut self, end: usize) -> Result<MarkdownComposedLine> {
        let tail = self.row(end)?;
        let header = self.line_header(end, &tail)?;
        let head = self.row(header)?;
        self.decode_line(end, tail, head)
    }
    /// Synchronous lookup and the quota-one cursor share pure decoding. This
    /// step performs no extra read that could hide two preads inside one quantum.
    fn decode_line(
        &self,
        end: usize,
        tail: [u8; 64],
        head: [u8; 64],
    ) -> Result<MarkdownComposedLine> {
        let header = self.line_header(end, &tail)?;
        let parts = usize_at(&tail, 8)?;
        if head[56] != LINE || head[58] > 1 || head[59..].iter().any(|byte| *byte != 0) {
            return Err(Error::InconsistentCoverage);
        }
        let kind = match head[57] {
            // Accept only the actual writer's finite line-kind domain.
            0 => MarkdownComposedLineKind::Allowed,
            1 => MarkdownComposedLineKind::Mandatory,
            2 => MarkdownComposedLineKind::Emergency,
            3 => MarkdownComposedLineKind::Overflow,
            4 => MarkdownComposedLineKind::TableRow,
            _ => return Err(Error::InconsistentCoverage),
        };
        let facts = MarkdownComposedLineFacts {
            source_utf16: usize_at(&head, 0)?..usize_at(&head, 8)?,
            paragraph_utf16: usize_at(&head, 16)?..usize_at(&head, 24)?,
            ordinal: usize_at(&head, 32)?,
            parts,
            glyphs: usize_at(&tail, 32)?,
            x_advance_26_6: i64_at(&tail, 16),
            y_advance_26_6: i64_at(&tail, 24),
            kind,
        };
        let previous = match u64_at(&tail, 48) {
            // Absence uses the canonical on-disk sentinel, not a guessed offset.
            NONE => None,
            _ => Some(usize_at(&tail, 48)?),
        };
        let skip = match (u64_at(&head, 40), u64_at(&head, 48)) {
            // The first line has no predecessor. Later skips must point into
            // the authenticated earlier prefix and agree with its ordinal rule.
            (NONE, NONE) if facts.ordinal == 0 => None,
            (record, ordinal) if record != NONE && ordinal != NONE && facts.ordinal != 0 => {
                let record = usize_at(&head, 40)?;
                let ordinal = usize_at(&head, 48)?;
                if ordinal != (facts.ordinal & (facts.ordinal - 1))
                    || record >= header
                    || previous.is_none_or(|previous| record > previous)
                    || (ordinal.checked_add(1) == Some(facts.ordinal) && Some(record) != previous)
                {
                    return Err(Error::InconsistentCoverage);
                }
                Some((record, ordinal))
            }
            _ => return Err(Error::InconsistentCoverage),
        };
        if facts.source_utf16.start > facts.source_utf16.end
            || facts.paragraph_utf16.start > facts.paragraph_utf16.end
            || (facts.ordinal == 0) != previous.is_none()
            || (facts.ordinal == 0 && facts.source_utf16.start != self.range.start)
            || (facts.source_utf16.is_empty()
                && (facts.source_utf16.start != self.range.end
                    || facts.source_utf16.end
                        != self
                            .payload
                            .as_ref()
                            .ok_or(Error::Poisoned)?
                            .source
                            .utf16_len()
                    || facts.kind != MarkdownComposedLineKind::Mandatory
                    || facts.parts != 1
                    || facts.glyphs != 0
                    || facts.x_advance_26_6 != 0
                    || facts.y_advance_26_6 != 0))
            || facts.source_utf16.start < self.range.start
            || facts.source_utf16.end > self.range.end
            || facts.source_utf16.start < facts.paragraph_utf16.start
            || facts.source_utf16.end > facts.paragraph_utf16.end
            || facts.ordinal >= self.count
            || usize_at(&tail, 40)? != facts.source_utf16.len()
            || previous.is_some_and(|record| record >= header)
        {
            // Require ordered Source/paragraph ranges and consistent line
            // history. An empty line is valid only for the actual terminal
            // mandatory break, with exactly one glyph-free zero-advance part.
            return Err(Error::InconsistentCoverage);
        }
        Ok(MarkdownComposedLine {
            output: Arc::downgrade(&self.identity),
            header,
            end,
            previous,
            skip,
            base_level: head[58],
            facts,
        })
    }
    /// Reauthenticate the original weak output and actual records, not only a
    /// caller's copied line facts or numerically equal generation.
    fn require_line(&mut self, line: &MarkdownComposedLine) -> Result<()> {
        if !Weak::ptr_eq(&line.output, &Arc::downgrade(&self.identity)) {
            return Err(Error::SourceMismatch);
        }
        let actual = self.line_at(line.end)?;
        if actual.header != line.header
            || actual.facts != line.facts
            || actual.previous != line.previous
            || actual.skip != line.skip
            || actual.base_level != line.base_level
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(())
    }
    /// Query the last sealed line; an empty original output returns None.
    pub fn last_line(&mut self) -> Result<Option<MarkdownComposedLine>> {
        let result = (|| {
            self.require(&self.authority)?;
            self.last.map(|record| self.line_at(record)).transpose()
        })();
        self.finish(result)
    }

    /// Read the paragraph base authenticated by this original line header.
    /// Reuse the existing two-record check; neither a first-part/minimum level
    /// guess nor another Bidi/Reader owner can supply the paragraph direction.
    pub fn paragraph_base_level(&mut self, line: &MarkdownComposedLine) -> Result<u8> {
        let result = (|| {
            self.require(&self.authority)?;
            self.require_line(line)?;
            Ok(line.base_level)
        })();
        self.finish(result)
    }
    /// Follow an authenticated predecessor and require contiguous source/ordinal
    /// coverage, retaining the same actual output across reverse traversal.
    pub fn previous_line(
        &mut self,
        line: &MarkdownComposedLine,
    ) -> Result<Option<MarkdownComposedLine>> {
        let result = (|| {
            self.require_line(line)?;
            let previous = line
                .previous
                .map(|record| self.line_at(record))
                .transpose()?;
            if let Some(previous) = &previous {
                // A valid record pointer still needs the exact previous ordinal
                // and adjoining Source boundary to establish line continuity.
                if previous.facts.ordinal.checked_add(1) != Some(line.facts.ordinal)
                    || previous.facts.source_utf16.end != line.facts.source_utf16.start
                {
                    return Err(Error::InconsistentCoverage);
                }
            } else if line.facts.ordinal != 0 {
                // Only the actual first line can legitimately lack a predecessor.
                return Err(Error::InconsistentCoverage);
            }
            Ok(previous)
        })();
        self.finish(result)
    }
    /// Read one authenticated line part through its exact metadata and item
    /// records. The ordinal immediately after the last part is ordinary EOF.
    pub fn part(
        &mut self,
        line: &MarkdownComposedLine,
        ordinal: usize,
    ) -> Result<Option<MarkdownComposedLinePart>> {
        let result = (|| {
            self.require_line(line)?;
            if ordinal > line.facts.parts {
                // Beyond one-past-end is invalid input, not an absent part.
                return Err(Error::InvalidRequest);
            }
            if ordinal == line.facts.parts {
                // One-past-end reads no unrelated record or Source bytes.
                return Ok(None);
            }
            let record = ordinal
                .checked_mul(2)
                .and_then(|value| line.header.checked_add(1)?.checked_add(value))
                .ok_or(Error::Overflow)?;
            self.require(&self.authority)?;
            let mut part_records = [[0; 64]; 2];
            // Both original part records are required for authentication. Read
            // that exact contiguous range without another part or extra cache.
            self.payload
                .as_mut()
                .ok_or(Error::Poisoned)?
                .reader
                .read_batch(
                    record..record.checked_add(2).ok_or(Error::Overflow)?,
                    &mut part_records,
                )?;
            let [row, meta] = part_records;
            Self::part_header(line, &row, &meta)?;
            let item_record = usize_at(&meta, 40)?;
            self.require(&self.authority)?;
            let mut item_records = [[0; 64]; 3];
            // Only the authenticated part can supply the original item pointer.
            // Read exactly three directory records, then check coverage/weak Font.
            self.payload
                .as_mut()
                .ok_or(Error::Poisoned)?
                .reader
                .read_batch(
                    item_record..item_record.checked_add(3).ok_or(Error::Overflow)?,
                    &mut item_records,
                )?;
            let item = decode_directory(item_records)?;
            self.decode_part(line, row, meta, item).map(Some)
        })();
        self.finish(result)
    }

    /// Authenticate both original part records before following the item pointer; both read paths share this format.
    fn part_header(line: &MarkdownComposedLine, row: &[u8; 64], meta: &[u8; 64]) -> Result<()> {
        if row[56] != PART
            || meta[56] != PART_META
            || row[57..].iter().any(|byte| *byte != 0)
            || meta[10..12].iter().any(|byte| *byte != 0)
            || u64_at(meta, 32) != number(line.facts.ordinal)?
        {
            return Err(Error::InconsistentCoverage);
        }
        Ok(())
    }

    /// Decode the five records already read; this conversion performs no additional IO.
    fn decode_part(
        &self,
        line: &MarkdownComposedLine,
        row: [u8; 64],
        meta: [u8; 64],
        item: Directory,
    ) -> Result<MarkdownComposedLinePart> {
        // Decode finite metadata from already-read records; no Native call or
        // live-document style lookup may reconstruct the original item facts.
        let font = FontKey {
            generation: u64_at(&meta, 0),
            slot: u16::from_le_bytes(meta[8..10].try_into().expect("固定2B字段")),
        };
        let direction = match meta[58] {
            0 => MarkdownShapingDirection::LeftToRight,
            1 => MarkdownShapingDirection::RightToLeft,
            _ => return Err(Error::InconsistentCoverage),
        };
        let facts = MarkdownComposedLinePartFacts {
            paragraph_inset_26_6: item.paragraph_inset_26_6,
            presentation: item.presentation,
            kind: super::super::MarkdownComposedLinePartKind::decode(meta[59])?,
            source_utf16: usize_at(&row, 0)?..usize_at(&row, 8)?,
            glyphs: usize_at(&row, 24)?,
            origin_x_26_6: i64_at(&row, 32),
            origin_y_26_6: i64_at(&row, 40),
            points: f32::from_le_bytes(meta[12..16].try_into().expect("固定4B字段")),
            x_advance_26_6: i64_at(&meta, 16),
            y_advance_26_6: i64_at(&meta, 24),
            level: meta[57],
            direction,
            script: meta[60..64].try_into().expect("固定4B字段"),
            visual_glyphs: usize_at(&meta, 48)?
                ..usize_at(&meta, 48)?
                    .checked_add(usize_at(&row, 24)?)
                    .ok_or(Error::Overflow)?,
        };
        let glyph_start = usize_at(&row, 16)?;
        let original_start = usize_at(&meta, 48)?;
        if facts.kind != item.kind
            || (facts.source_utf16.is_empty()
                && (facts.kind != super::super::MarkdownComposedLinePartKind::ParagraphBreak
                    || facts.source_utf16.end != self.range.end))
            || (facts.kind != super::super::MarkdownComposedLinePartKind::Glyphs
                && facts.glyphs != 0)
            || facts.source_utf16.start < line.facts.source_utf16.start
            || facts.source_utf16.end > line.facts.source_utf16.end
            || facts.source_utf16.start < item.facts.source_utf16.start
            || facts.source_utf16.end > item.facts.source_utf16.end
            || font != item.font
            || u64_at(&row, 48) != item.origin
            || facts.script != item.facts.script
            || facts.points != item.facts.points
            || !facts.points.is_finite()
            || (facts.points <= 0.0
                && facts.kind != super::super::MarkdownComposedLinePartKind::TableRow)
            || facts.level > 126
            || facts.level & 1 != u8::from(direction == MarkdownShapingDirection::RightToLeft)
            || original_start
                .checked_add(facts.glyphs)
                .is_none_or(|end| end > item.glyph_count)
            || item.glyph_start.checked_add(original_start) != Some(glyph_start)
        {
            // Part kind, ranges, glyph extent, font, and Bidi parity must all
            // agree with the original item before its registration is exposed.
            return Err(Error::InconsistentCoverage);
        }
        let registration = self
            .payload
            .as_ref()
            .ok_or(Error::Poisoned)?
            .fonts
            .resolve(font)?
            .registration();
        Ok(MarkdownComposedLinePart {
            output: Arc::downgrade(&self.identity),
            source: self.authority.clone(),
            registration,
            glyph_start,
            cluster_start: item.cluster_start,
            cluster_count: item.cluster_count,
            facts,
        })
    }
    /// Read original glyph metadata only. Range/font/output-ticket failure
    /// revokes the candidate; no output batch may be published after Err.
    pub fn read_batch(
        &mut self,
        part: &MarkdownComposedLinePart,
        range: Range<usize>,
        output: &mut [MarkdownShapedGlyph],
    ) -> Result<()> {
        let result = (|| {
            self.require(&part.source)?;
            if !Weak::ptr_eq(&part.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            let payload = self.payload.as_mut().ok_or(Error::Poisoned)?;
            payload.fonts.require_registration(&part.registration)?;
            if range.end.checked_sub(range.start) != Some(output.len())
                || output.len() > STEP
                || range.end > part.facts.glyphs
            {
                return Err(Error::InvalidRequest);
            }
            let start = part
                .glyph_start
                .checked_add(range.start)
                .ok_or(Error::Overflow)?;
            let end = part
                .glyph_start
                .checked_add(range.end)
                .ok_or(Error::Overflow)?;
            if output.is_empty() {
                // Even an empty draw batch checks the same Reader's health, FD,
                // and exact empty extent. Skip only the unused glyph scratch;
                // an empty request cannot bypass authorization by returning early.
                payload.reader.read_batch(start..end, &mut [])?;
            } else {
                // A finite stack batch reads actual records, then admits each
                // decoded cluster only inside this original part's Source range.
                let mut rows = [[0; 64]; STEP];
                payload
                    .reader
                    .read_batch(start..end, &mut rows[..output.len()])?;
                for (row, slot) in rows.into_iter().zip(output.iter_mut()) {
                    let glyph = decode_glyph(row)?;
                    if !part
                        .facts
                        .source_utf16
                        .contains(&glyph.source_utf16_cluster)
                    {
                        return Err(Error::InconsistentCoverage);
                    }
                    *slot = glyph;
                }
            }
            payload.fonts.require_registration(&part.registration)?;
            Ok(())
        })();
        self.finish(result)
    }

    /// Search the original item cluster directory without scanning document text
    /// or materializing every cluster in the paragraph.
    fn original_cluster(
        &mut self,
        part: &MarkdownComposedLinePart,
        offset: usize,
    ) -> Result<MarkdownGlyphCluster> {
        let (mut low, mut high) = (0, part.cluster_count);
        while low < high {
            let middle = low + (high - low) / 2;
            let value = decode_cluster(
                self.row(
                    part.cluster_start
                        .checked_add(middle)
                        .ok_or(Error::Overflow)?,
                )?,
            )?;
            if offset < value.source_utf16.start {
                // Continue only into the earlier half of this ordered directory.
                high = middle;
            } else if offset >= value.source_utf16.end {
                // A cluster ends before this offset; search the remaining suffix.
                low = middle + 1;
            } else {
                // The located cluster must also belong to this exact soft-line
                // part, including its original visual-glyph interval.
                if value.source_utf16.start < part.facts.source_utf16.start
                    || value.source_utf16.end > part.facts.source_utf16.end
                    || value.glyphs.start < part.facts.visual_glyphs.start
                    || value.glyphs.end > part.facts.visual_glyphs.end
                {
                    return Err(Error::InconsistentCoverage);
                }
                return Ok(value);
            }
        }
        Err(Error::InconsistentCoverage)
    }

    /// Preserve original Source/flags/advances while rebasing glyph and logical
    /// prefix coordinates to this actual part, without scanning document text.
    pub fn cluster(
        &mut self,
        part: &MarkdownComposedLinePart,
        offset: usize,
    ) -> Result<MarkdownGlyphClusterAnswer> {
        let result = (|| {
            self.require(&part.source)?;
            if !Weak::ptr_eq(&part.output, &Arc::downgrade(&self.identity)) {
                return Err(Error::SourceMismatch);
            }
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&part.registration)?;
            if offset < part.facts.source_utf16.start || offset > part.facts.source_utf16.end {
                return Err(Error::InvalidRequest);
            }
            if offset == part.facts.source_utf16.end {
                // The exact part endpoint is known absence, not a query against
                // an adjacent item or an unprepared document suffix.
                return Ok(MarkdownGlyphClusterAnswer::Known(None));
            }
            if part.facts.kind != super::super::MarkdownComposedLinePartKind::Glyphs {
                // Original control ranges carry Source/actual advance only;
                // they invent neither drawable glyphs nor internal ligature stops.
                return Ok(MarkdownGlyphClusterAnswer::Known(Some(
                    MarkdownGlyphCluster {
                        source_utf16: part.facts.source_utf16.clone(),
                        glyphs: 0..0,
                        missing_glyphs: 0,
                        flags: 0,
                        prefix_x_26_6: 0,
                        prefix_y_26_6: 0,
                        x_advance_26_6: part.facts.x_advance_26_6,
                        y_advance_26_6: 0,
                    },
                )));
            }
            let mut value = self.original_cluster(part, offset)?;
            let start = self.original_cluster(part, part.facts.source_utf16.start)?;
            // Item prefixes accumulate from the logical item start. Rebase to
            // this part so a later wrapped line's caret is not shifted by earlier lines.
            value.prefix_x_26_6 = value
                .prefix_x_26_6
                .checked_sub(start.prefix_x_26_6)
                .ok_or(Error::Overflow)?;
            value.prefix_y_26_6 = value
                .prefix_y_26_6
                .checked_sub(start.prefix_y_26_6)
                .ok_or(Error::Overflow)?;
            value.glyphs = value
                .glyphs
                .start
                .checked_sub(part.facts.visual_glyphs.start)
                .ok_or(Error::Overflow)?
                ..value
                    .glyphs
                    .end
                    .checked_sub(part.facts.visual_glyphs.start)
                    .ok_or(Error::Overflow)?;
            self.payload
                .as_ref()
                .ok_or(Error::Poisoned)?
                .fonts
                .require_registration(&part.registration)?;
            Ok(MarkdownGlyphClusterAnswer::Known(Some(value)))
        })();
        self.finish(result)
    }

    /// Return the actual reader and then inspect the same Source health. Source
    /// failure keeps its typed priority; release observers retain file cleanup
    /// facts even when both operations fail.
    pub fn close(mut self) -> Result<()> {
        let Some(payload) = self.payload.take() else {
            // A prior terminal query already returned the reader; it cannot
            // yield a successful close receipt for a now-absent payload.
            return Err(Error::Poisoned);
        };
        let result = payload.reader.close().map_err(Error::from);
        payload.source.health()?;
        result
    }
}
