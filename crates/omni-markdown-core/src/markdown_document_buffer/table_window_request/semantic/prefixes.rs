//! Restore container styles and source markers from actual scope, never inferred projection ancestry.

use super::*;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationRun, MarkdownPresentationStyles as Styles,
};
use pulldown_cmark::block_frontier::TableScopeKind;

const MAX_RUNS: usize = 8192;
pub(super) use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowQuota as BatchQuota;

#[derive(Default)]
pub(super) struct ScopeProjection {
    // Retain only actual ancestor starts and bounded visible-header marker facts.
    // The same batch quota owns their logical metadata and eventual prefix runs.
    first: [Option<usize>; 3],
    header_markers: Vec<(TableScopeKind, usize)>,
    before_header: [bool; 3],
    quote_starts: Vec<usize>,
    quote_count: usize,
    quota: Option<BatchQuota>,
}
impl ScopeProjection {
    pub fn with_quota(quota: BatchQuota) -> Self {
        Self {
            quota: Some(quota),
            ..Self::default()
        }
    }
    pub fn observe(
        &mut self,
        ticket: &MarkdownTableStructuralWindow,
        kind: TableScopeKind,
        start: usize,
    ) -> Result<usize, Error> {
        // Scope kinds outside quote/list semantics contribute no prefix styles;
        // each accepted kind still needs its original start before table content.
        let Some(index) = kind_index(kind) else {
            return Ok(0);
        };
        let header = ticket.header;
        if start > header.header_byte_start + header.header_prefix_bytes {
            // An ancestor must already be open before table content; a future
            // container cannot acquire styling authority over this earlier row.
            return Err(Error::InvalidSource);
        }
        self.first[index] = Some(self.first[index].map_or(start, |old| old.min(start)));
        if kind == TableScopeKind::BlockQuote {
            if self.quote_count == MAX_RUNS {
                return Err(Error::SemanticLimit);
            }
            self.quote_count += 1;
            if !ticket.include_header {
                // Offscreen headers need no opening directory. Continuation
                // prefixes require only the actual fixed quote depth.
                return Ok(0);
            }
            // Visible headers retain every real quote start; merging by kind
            // or scanning literal '>' inside footnote labels would lose ownership.
            if self.quote_starts.len() + self.header_markers.len() == MAX_RUNS {
                return Err(Error::SemanticLimit);
            }
            if let Some(quota) = &self.quota {
                quota.admit_metadata(std::mem::size_of::<usize>())?;
            }
            self.quote_starts.push(start);
            return Ok(std::mem::size_of::<usize>());
        }
        if !ticket.include_header {
            return Ok(0);
        }
        let anchor = if start < header.header_byte_start {
            if self.before_header[index] {
                return Ok(0);
            } // Older ancestors of one kind share the header's actual physical marker start.
            self.before_header[index] = true;
            header.header_byte_start
        } else {
            start
        };
        if self.header_markers.len() + self.quote_starts.len() == MAX_RUNS {
            // Admit metadata before push. A deep spine cannot create an
            // unbounded header-marker directory before reporting its limit.
            return Err(Error::SemanticLimit);
        }
        if let Some(quota) = &self.quota {
            quota.admit_metadata(std::mem::size_of::<(TableScopeKind, usize)>())?;
        }
        self.header_markers.push((kind, anchor));
        Ok(std::mem::size_of::<(TableScopeKind, usize)>())
    }
    pub fn into_builder(self) -> PrefixRuns {
        // Transfer these same original scope facts to one serial run builder;
        // no second ancestor directory or copied source body is introduced.
        PrefixRuns {
            scope: self,
            runs: Vec::new(),
            first_index: 0,
            row_index: 0,
            marker_index: 0,
            quote_index: 0,
            quote_cursor: 0,
            stage: Stage::Containers,
        }
    }
}

#[derive(Clone, Copy)]
enum Stage {
    Containers,
    TablePrefix,
    Quotes,
    Markers,
    Complete,
}

pub(super) struct PrefixRuns {
    scope: ScopeProjection,
    runs: Vec<MarkdownPresentationRun>,
    first_index: usize,
    row_index: usize,
    marker_index: usize,
    quote_index: usize,
    quote_cursor: usize,
    stage: Stage,
}
impl PrefixRuns {
    /// Map one bounded style/marker per step, reporting the existing marker recognizer's input separately.
    pub fn advance(
        &mut self,
        lines: &[OwnedLine],
        ticket: &MarkdownTableStructuralWindow,
        selection: &Range<usize>,
        document_utf16_len: usize,
    ) -> Result<(SemanticWork, bool), Error> {
        let mut work = SemanticWork {
            structural_transitions: 64,
            ..SemanticWork::default()
        };
        match self.stage {
            Stage::Containers => {
                // Container extent starts at its actual ancestor or the first
                // retained line. Convert only the bounded real prefix to UTF-16.
                if self.first_index == 3 {
                    self.stage = Stage::TablePrefix;
                    return Ok((work, false));
                }
                let index = self.first_index;
                self.first_index += 1;
                if let Some(start) = self.scope.first[index] {
                    let h = &lines[0];
                    let clipped = start.max(h.spec.bytes.start);
                    let local = clipped - h.spec.bytes.start;
                    let prefix = h.text.get(..local).ok_or(Error::InvalidSource)?;
                    work.normalizer_input_bytes = prefix.len();
                    let utf16_start = h.spec.utf16.start + prefix.encode_utf16().count();
                    let end = lines.last().ok_or(Error::InvalidSource)?.spec.utf16.end;
                    self.push(utf16_start..end, style(index))?;
                }
            }
            Stage::TablePrefix => {
                if self.row_index == lines.len() {
                    self.row_index = 0;
                    self.stage = Stage::Quotes;
                    return Ok((work, false));
                }
                let line = &lines[self.row_index];
                let is_header = self.row_index == 0;
                self.row_index += 1;
                if is_header
                    || line.spec.prefix == 0
                    || line.spec.delimiter && !ticket.include_header
                {
                    return Ok((work, false)); // A header prefix precedes the actual table start and must not gain TABLE.
                }
                let prefix = line
                    .text
                    .get(..line.spec.prefix)
                    .ok_or(Error::InvalidSource)?;
                work.normalizer_input_bytes = prefix.len();
                let mut styles = Styles::TABLE;
                if line.spec.delimiter {
                    // The original delimiter extent includes its container
                    // prefix and excludes EOL; restore those exact hidden properties.
                    styles = styles
                        .union(Styles::TABLE_DELIMITER)
                        .union(Styles::HIDDEN_MARKER);
                }
                self.push(
                    line.spec.utf16.start..line.spec.utf16.start + prefix.encode_utf16().count(),
                    styles,
                )?;
            }
            Stage::Quotes => {
                if self.row_index == lines.len() {
                    // Continue into the original list-marker stage after quotes;
                    // a step still emits at most one actual marker.
                    self.row_index = 0;
                    self.stage = Stage::Markers;
                    return Ok((work, false));
                }
                let line = &lines[self.row_index];
                if self.quote_index == self.scope.quote_count
                    || self.row_index <= 1 && !ticket.include_header
                {
                    self.row_index += 1;
                    self.quote_index = 0;
                    self.quote_cursor = 0;
                    return Ok((work, false));
                }
                let source_start = self.scope.quote_starts.get(self.quote_index).copied();
                self.quote_index += 1;
                let prefix = line
                    .text
                    .get(..line.spec.prefix)
                    .ok_or(Error::InvalidSource)?;
                let start = if self.row_index == 0
                    && source_start.is_some_and(|start| start >= line.spec.bytes.start)
                {
                    // A new header quote starts at its verified anchor, past the
                    // actual item/footnote opening, not a literal '>' in its label.
                    source_start.ok_or(Error::InvalidSource)? - line.spec.bytes.start
                } else {
                    // Table continuation contains only the original container
                    // prefix; follow scope order from the preceding quote's end.
                    self.quote_cursor
                };
                if start < self.quote_cursor || start > prefix.len() {
                    return Err(Error::InvalidSource);
                }
                work.normalizer_input_bytes = prefix.len() - start;
                let range =
                    crate::markdown_viewport_presentation::parser::table_line_prefix_marker(
                        prefix,
                        start,
                        TableScopeKind::BlockQuote,
                    )
                    .map_err(|_| Error::ParserMismatch)?
                    .ok_or(Error::ParserMismatch)?;
                self.quote_cursor = range.end;
                let marker_start =
                    line.spec.utf16.start + prefix[..range.start].encode_utf16().count();
                let marker_end = marker_start + prefix[range].encode_utf16().count();
                let includes_end =
                    line.spec.utf16.end == document_utf16_len && !line.text.ends_with(['\r', '\n']);
                let active =
                    crate::markdown_viewport_presentation::markdown_physical_line_is_active(
                        selection,
                        &line.spec.utf16,
                        includes_end,
                    );
                let styles = if active {
                    Styles::MARKER
                } else {
                    // Use the actual physical-line endpoint rule; a compressed
                    // projection or gap cannot make an inactive quote marker visible.
                    Styles::MARKER.union(Styles::HIDDEN_MARKER)
                };
                self.push(marker_start..marker_end, styles)?;
            }
            Stage::Markers => {
                if self.row_index == lines.len() {
                    self.stage = Stage::Complete;
                    return Ok((work, true));
                }
                let line = &lines[self.row_index];
                if self.row_index <= 1 && !ticket.include_header {
                    self.row_index += 1;
                    self.marker_index = 0;
                    return Ok((work, false)); // Offscreen header/delimiter are syntax context, not published markers.
                }
                let marker = if self.row_index == 0 {
                    self.scope.header_markers.get(self.marker_index).copied()
                } else if self.marker_index < 3 {
                    self.scope.first[self.marker_index]
                        .map(|_| (kind(self.marker_index), line.spec.bytes.start))
                } else {
                    None
                };
                let limit = if self.row_index == 0 {
                    self.scope.header_markers.len()
                } else {
                    3
                };
                if self.marker_index == limit {
                    self.row_index += 1;
                    self.marker_index = 0;
                    return Ok((work, false));
                }
                self.marker_index += 1;
                if let Some((role, start)) = marker {
                    if role == TableScopeKind::BlockQuote {
                        return Ok((work, false)); // Each quote layer was already emitted from its own original authority.
                    }
                    let local_start = start.max(line.spec.bytes.start) - line.spec.bytes.start;
                    let prefix = line
                        .text
                        .get(..line.spec.prefix)
                        .ok_or(Error::InvalidSource)?;
                    if local_start > prefix.len() {
                        return Err(Error::InvalidSource);
                    }
                    // Recognize only the known bounded container prefix. Marker
                    // recovery cannot authorize reading/parsing an entire huge cell.
                    work.normalizer_input_bytes = prefix.len();
                    if let Some(range) =
                        crate::markdown_viewport_presentation::parser::table_line_prefix_marker(
                            prefix,
                            local_start,
                            role,
                        )
                        .map_err(|_| Error::ParserMismatch)?
                    {
                        let start =
                            line.spec.utf16.start + prefix[..range.start].encode_utf16().count();
                        let end = start + prefix[range].encode_utf16().count();
                        self.push(start..end, Styles::MARKER)?;
                    }
                }
            }
            Stage::Complete => return Ok((work, true)),
        }
        Ok((work, false))
    }
    fn push(&mut self, range: Range<usize>, styles: Styles) -> Result<(), Error> {
        // Empty ranges retain no run. Admit the actual new prefix before it
        // joins the bounded output, sharing the same quota as final projection.
        if range.start >= range.end {
            return Ok(());
        }
        if self.runs.len() == MAX_RUNS {
            return Err(Error::SemanticLimit);
        }
        if let Some(quota) = &self.scope.quota {
            quota.admit_prefix()?;
        }
        self.runs.push(MarkdownPresentationRun {
            utf16_range: range,
            styles,
        });
        Ok(())
    }
    pub fn into_runs(self) -> Vec<MarkdownPresentationRun> {
        self.runs
    }
}
fn kind_index(kind: TableScopeKind) -> Option<usize> {
    match kind {
        TableScopeKind::BlockQuote => Some(0),
        TableScopeKind::OrderedList => Some(1),
        TableScopeKind::UnorderedList => Some(2),
        TableScopeKind::Other => None,
    }
}
fn kind(index: usize) -> TableScopeKind {
    [
        TableScopeKind::BlockQuote,
        TableScopeKind::OrderedList,
        TableScopeKind::UnorderedList,
    ][index]
}
fn style(index: usize) -> Styles {
    [
        Styles::BLOCK_QUOTE,
        Styles::ORDERED_LIST,
        Styles::UNORDERED_LIST,
    ][index]
}
