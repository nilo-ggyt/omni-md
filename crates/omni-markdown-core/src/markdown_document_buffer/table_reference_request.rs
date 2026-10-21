//! A bounded parsing transaction for one actual global definition. Retain no
//! strong document/root owner and build no document-wide reference directory.
//!
//! The caller must supply facts from the global producer driven by this same
//! Source identity, in source order. This module rechecks weak root/generation,
//! but the opaque fact has no cross-document root brand and cannot itself prove
//! that a caller paired it with the correct document. Every Err must fail the
//! outer global request: never skip it to use a later same-name definition or
//! declare an unresolved value Missing.

use super::{MarkdownDocumentBuffer, table_source::MarkdownTableSourceIdentity};
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowError as Error, TABLE_WINDOW_MAX_REFERENCE_BYTES,
    references::MarkdownTableReferenceQueries as Queries,
};
use pulldown_cmark::{
    block_frontier::SourceFrame,
    global_frontier::{
        GlobalSourceAuthority, ReferencePrefixPurpose, ReferencePrefixScanner, ReferencePrefixStep,
        VerifiedGlobalReference,
    },
};
use std::ops::Range;

// Reserve control/cleanup work for the same Source owner. The actual Scanner
// still requires at least 2048 available work units after those reservations.
const MIN_STEP: usize = 3 * 1_024;
const MAX_STEP: usize = 64 * 1_024;
const STATE_WORK: usize = 1_024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Report source, copy and structural work separately. The normalizer and Parser
/// are bounded whole-slice oracles whose input sizes are accounted independently;
/// their internal AST/Unicode work is not claimed as token-counted fuel here.
pub(super) struct ReferenceWork {
    pub source_bytes: usize,
    pub copied_bytes: usize,
    pub metadata_bytes: usize,
    pub source_tree_steps: usize,
    pub structural_transitions: usize,
    pub normalizer_input_bytes: usize,
    pub parser_input_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReferenceStep {
    pub work: ReferenceWork,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    // Label matching finishes before the definition payload is admitted. Each
    // phase is resumed by the original request instead of synchronously scanning
    // a deep continuation scope or copying an unrelated URL/title in one step.
    CopyLabel,
    ScanLabel,
    ProbeLabel,
    NormalizeLabel,
    CopyDefinition,
    ScanDefinition,
    ProbeDefinition,
    StripDefinition,
    DecodeDefinition,
    Complete,
}

#[derive(Clone, Copy, Debug)]
/// Retain one actual continuation-line position and verified prefix skip, without
/// keeping line text or flattening the deep container scope into another role list.
struct PrefixRecord {
    at: usize,
    bytes: usize,
}

struct Buffer {
    range: Range<usize>,
    text: String,
    scan_at: usize,
    prefixes: Vec<PrefixRecord>,
}

impl Buffer {
    fn new(range: Range<usize>, maximum: usize) -> Result<Self, Error> {
        let len = range
            .end
            .checked_sub(range.start)
            .ok_or(Error::InvalidSource)?;
        if len == 0 || len > maximum {
            // Admit the full range before reserving. A truncated label or URL
            // cannot receive the original normalizer's complete-input EOF semantics.
            return Err(Error::ReferenceLimit);
        }
        Ok(Self {
            range,
            text: String::with_capacity(len),
            scan_at: 0,
            prefixes: Vec::new(),
        })
    }
}

pub(super) struct Request {
    source: MarkdownTableSourceIdentity,
    fact: Option<VerifiedGlobalReference>,
    buffer: Option<Buffer>,
    probe: Option<ReferencePrefixScanner>,
    pending_line: Option<usize>,
    normalized: Option<String>,
    stripped: Option<String>,
    strip_at: usize,
    strip_record: usize,
    phase: Phase,
    failure: Option<Error>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Default diagnostics expose state and bounded capacities only, never a
        // label, URL, title, or personal source bytes.
        formatter
            .debug_struct("ReferenceRequest")
            .field("source", &self.source)
            .field("phase", &self.phase)
            .field("failure", &self.failure)
            .field(
                "source_buffer_bytes",
                &self.buffer.as_ref().map_or(0, |buffer| buffer.text.len()),
            )
            .field(
                "prefix_records",
                &self
                    .buffer
                    .as_ref()
                    .map_or(0, |buffer| buffer.prefixes.len()),
            )
            .finish()
    }
}

impl Request {
    /// Copy no text when ASCII counts rule out a match. A possible-match range
    /// exceeding its allowance is unresolved failure, not a confirmed miss.
    pub(super) fn new(
        source: MarkdownTableSourceIdentity,
        fact: VerifiedGlobalReference,
        queries: &Queries,
    ) -> Result<Option<Self>, Error> {
        let candidate = fact.candidate();
        let label_end = candidate
            .label_end
            .checked_add(1)
            .ok_or(Error::InvalidSource)?;
        if candidate.source_revision != source.generation()
            || candidate.span_start > candidate.label_start
            || candidate.label_start >= label_end
            || label_end > candidate.span_end
            || candidate.span_end > source.byte_len()
        {
            // The current Source must have driven the fact. Even unrelated labels
            // cannot admit reversed ranges or facts from another generation.
            return Err(Error::InvalidSource);
        }
        if !queries.may_match_ascii_count(candidate.label_ordinary_ascii) {
            return Ok(None); // An unrelated giant URL/title needs no read, allocation, or finite value-inventory slot.
        }
        let buffer = Buffer::new(
            candidate.label_start..label_end,
            queries.maximum_candidate_label_bytes(),
        )?;
        Ok(Some(Self {
            source,
            fact: Some(fact),
            buffer: Some(buffer),
            probe: None,
            pending_line: None,
            normalized: None,
            stripped: None,
            strip_at: 0,
            strip_record: 0,
            phase: Phase::CopyLabel,
            failure: None,
        }))
    }

    /// Execute one phase per step, reserving the source tree's cached height and
    /// fixed state/shared-scope completion cost. The outer request checks cancel
    /// before calling; every entry here, including Complete, rechecks root/generation.
    pub(super) fn advance(
        &mut self,
        document: &MarkdownDocumentBuffer,
        queries: &mut Queries,
        budget: usize,
    ) -> Result<ReferenceStep, Error> {
        if let Some(error) = self.failure {
            return Err(error); // A later short definition or another frame delivery cannot repair the first failure.
        }
        let result = self.advance_inner(document, queries, budget);
        if let Err(error) = result {
            // Revoke the original query inventory too. Ignoring Err must not let
            // EOF or another Request publish a partial resolution afterward.
            queries.fail_verified_resolution();
            self.failure = Some(error);
            self.release_payload();
        }
        result
    }

    fn advance_inner(
        &mut self,
        document: &MarkdownDocumentBuffer,
        queries: &mut Queries,
        budget: usize,
    ) -> Result<ReferenceStep, Error> {
        if !self.source.is_current(document) {
            return Err(Error::InvalidSource);
        }
        if !(MIN_STEP..=MAX_STEP).contains(&budget) {
            return Err(Error::SourceLimit);
        }
        let tree_work = document
            .root
            .as_ref()
            .map_or(0, |root| usize::from(root.height()))
            + 1;
        let available = budget
            .checked_sub(STATE_WORK + tree_work)
            .ok_or(Error::SourceLimit)?;
        if available < 2_048 {
            return Err(Error::SourceLimit); // A deep source tree cannot consume the prefix Scanner's actual minimum fuel.
        }
        let mut work = ReferenceWork {
            structural_transitions: STATE_WORK,
            ..ReferenceWork::default()
        };
        match self.phase {
            Phase::CopyLabel | Phase::CopyDefinition => {
                self.copy_source(document, available, tree_work, &mut work)?;
            }
            Phase::ScanLabel | Phase::ScanDefinition => self.scan_lines(available, &mut work)?,
            Phase::ProbeLabel | Phase::ProbeDefinition => {
                self.advance_prefix(document, queries, available, tree_work, &mut work)?;
            }
            Phase::NormalizeLabel => self.normalize(queries, &mut work)?,
            Phase::StripDefinition => self.strip(available, &mut work)?,
            Phase::DecodeDefinition => {
                let label = self.normalized.as_deref().ok_or(Error::InvalidSource)?;
                let value = self.stripped.as_deref().ok_or(Error::InvalidSource)?;
                // The original Parser receives only a matched complete definition
                // of at most 16 KiB, retaining entity/escape/title/first-definition
                // semantics. parser_input_bytes bounds this independent oracle's
                // input, not a claim that its internal tokens obey `available`.
                work.parser_input_bytes = value.len();
                if queries.admit_verified_definition(label, value)? {
                    // Oracle input remains separately bounded. Account for the
                    // actual Arc<String> ownership header added to the destination
                    // without pretending the whole Parser is token-metered here.
                    work.metadata_bytes += crate::MarkdownLinkDestination::whole_metadata();
                }
                self.release_payload();
                self.phase = Phase::Complete;
            }
            Phase::Complete => (), // Completion does not renew an old Source or recreate released payload.
        }
        debug_assert!(work.source_bytes <= budget);
        debug_assert!(work.structural_transitions + work.source_tree_steps <= budget);
        Ok(ReferenceStep {
            work,
            complete: self.phase == Phase::Complete,
        })
    }

    fn copy_source(
        &mut self,
        document: &MarkdownDocumentBuffer,
        available: usize,
        tree_work: usize,
        work: &mut ReferenceWork,
    ) -> Result<(), Error> {
        let buffer = self.buffer.as_mut().ok_or(Error::InvalidSource)?;
        let next = buffer.range.start + buffer.text.len();
        if next == buffer.range.end {
            self.phase = if self.phase == Phase::CopyLabel {
                Phase::ScanLabel
            } else {
                Phase::ScanDefinition
            };
            return Ok(());
        }
        // Reserve one tree descent, up to two UTF-8 validations and one copy.
        // Frames may split scalars, but the retained buffer always ends on a
        // scalar boundary and the next step resumes at that exact source byte.
        let limit = (available / 3).min(buffer.range.end - next);
        let frame = document
            .table_source_frame(next, limit)
            .map_err(|_| Error::InvalidSource)?;
        work.source_tree_steps = tree_work;
        work.source_bytes = frame.bytes.len();
        let text = match std::str::from_utf8(&frame.bytes) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
                // Defer only the incomplete scalar's at-most-three trailing bytes.
                // Reborrow at the exact position next step; never replace/drop bytes.
                std::str::from_utf8(&frame.bytes[..error.valid_up_to()])
                    .map_err(|_| Error::InvalidSource)?
            }
            Err(_) => return Err(Error::InvalidSource),
        };
        if text.is_empty() {
            return Err(Error::InvalidSource); // A nonempty range without progress must not enter a busy loop.
        }
        work.structural_transitions += frame.bytes.len() * 3;
        work.copied_bytes = text.len();
        buffer.text.push_str(text);
        frame.finish().map_err(|_| Error::InvalidSource)?;
        Ok(())
    }

    fn scan_lines(&mut self, available: usize, work: &mut ReferenceWork) -> Result<(), Error> {
        let buffer = self.buffer.as_mut().ok_or(Error::InvalidSource)?;
        let bytes = buffer.text.as_bytes();
        let mut used = 0;
        while buffer.scan_at < bytes.len() && used + 2 <= available {
            let at = buffer.scan_at;
            let byte = bytes[at];
            let width = if byte == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            buffer.scan_at += width;
            used += 2;
            work.metadata_bytes += width;
            if matches!(byte, b'\r' | b'\n') && buffer.scan_at < bytes.len() {
                // Identify only the actual next-line anchor. The original prefix
                // Scanner carried by the opaque fact owns container/whitespace rules.
                self.pending_line = Some(buffer.range.start + buffer.scan_at);
                self.phase = if self.phase == Phase::ScanLabel {
                    Phase::ProbeLabel
                } else {
                    Phase::ProbeDefinition
                };
                break;
            }
        }
        work.structural_transitions += used;
        if buffer.scan_at == bytes.len() && self.pending_line.is_none() {
            self.phase = if self.phase == Phase::ScanLabel {
                Phase::NormalizeLabel
            } else {
                Phase::StripDefinition
            };
        }
        Ok(())
    }

    fn advance_prefix(
        &mut self,
        document: &MarkdownDocumentBuffer,
        queries: &Queries,
        available: usize,
        tree_work: usize,
        work: &mut ReferenceWork,
    ) -> Result<(), Error> {
        let line = self.pending_line.ok_or(Error::InvalidSource)?;
        if self.probe.is_none() {
            let purpose = if self.phase == Phase::ProbeLabel {
                ReferencePrefixPurpose::Label
            } else {
                ReferencePrefixPurpose::ContainerOnly
            };
            self.probe = Some(
                self.fact
                    .as_ref()
                    .ok_or(Error::InvalidSource)?
                    .prefix_scanner(line, purpose)
                    .map_err(|_| Error::ParserMismatch)?,
            );
            return Ok(()); // Separate cursor creation from source reads; deep-scope expansion cannot hide inside that setup.
        }
        let probe = self.probe.as_mut().ok_or(Error::InvalidSource)?;
        let workspace_before = probe.allocated_workspace_bytes();
        let frame = document
            .table_source_frame(probe.next_byte(), available)
            .map_err(|_| Error::InvalidSource)?;
        work.source_tree_steps = tree_work;
        let step = probe
            .advance(
                SourceFrame {
                    source_revision: frame.generation,
                    byte_start: frame.byte_start,
                    bytes: &frame.bytes,
                    real_eof: frame.real_eof,
                },
                GlobalSourceAuthority {
                    current_revision: document.generation(),
                    cancelled: false,
                },
                available,
            )
            .map_err(|_| Error::ParserMismatch);
        // Consume the result only after returning the actual frame. Early Parser
        // completion must not conceal a mapping/reclaim failure.
        frame.finish().map_err(|_| Error::InvalidSource)?;
        let step = step?;
        let allocated = probe.allocated_workspace_bytes() - workspace_before;
        queries.admit_resolution_metadata(allocated)?;
        work.metadata_bytes += allocated;
        let (child_work, completed) = match step {
            ReferencePrefixStep::Progress { work, .. }
            | ReferencePrefixStep::NeedInput { work, .. } => (work, None),
            ReferencePrefixStep::Complete { prefix_bytes, work } => (work, Some(prefix_bytes)),
        };
        work.source_bytes = child_work.byte_reads;
        work.structural_transitions += child_work.transitions;
        if let Some(prefix) = completed {
            let buffer = self.buffer.as_mut().ok_or(Error::InvalidSource)?;
            let relative = line
                .checked_sub(buffer.range.start)
                .ok_or(Error::InvalidSource)?;
            let end = relative.checked_add(prefix).ok_or(Error::InvalidSource)?;
            if end > buffer.text.len()
                || !buffer.text.is_char_boundary(end)
                || buffer.prefixes.len() >= buffer.text.len()
            {
                return Err(Error::ParserMismatch);
            }
            if buffer.prefixes.len() == buffer.prefixes.capacity() {
                // Grow by one record per actual continuation line, not one slot
                // per raw byte merely because a line break was encountered.
                let before = buffer.prefixes.capacity();
                buffer.prefixes.reserve_exact(1);
                let allocated =
                    (buffer.prefixes.capacity() - before) * std::mem::size_of::<PrefixRecord>();
                queries.admit_resolution_metadata(allocated)?;
                work.metadata_bytes += allocated;
            }
            buffer.prefixes.push(PrefixRecord {
                at: line,
                bytes: prefix,
            });
            self.probe = None;
            self.pending_line = None;
            self.phase = if self.phase == Phase::ProbeLabel {
                Phase::ScanLabel
            } else {
                Phase::ScanDefinition
            };
        }
        Ok(())
    }

    fn normalize(&mut self, queries: &Queries, work: &mut ReferenceWork) -> Result<(), Error> {
        let buffer = self.buffer.as_ref().ok_or(Error::InvalidSource)?;
        work.normalizer_input_bytes = buffer.text.len();
        let normalized = self
            .fact
            .as_ref()
            .ok_or(Error::InvalidSource)?
            .normalized_label(&buffer.text, |after_eol| {
                // The normalizer supplies a suffix of this same raw buffer. Query
                // only the completed prefix map instead of synchronously traversing
                // deep scope again for every continuation line.
                let relative = buffer.text.len().checked_sub(after_eol.len())?;
                let at = buffer.range.start.checked_add(relative)?;
                let index = buffer
                    .prefixes
                    .binary_search_by_key(&at, |record| record.at)
                    .ok()?;
                Some(buffer.prefixes[index].bytes)
            })
            .map_err(|_| Error::ParserMismatch)?;
        if !queries.needs_definition(&normalized)? {
            self.release_payload();
            self.phase = Phase::Complete;
            return Ok(()); // An unrelated/already-resolved label stops here without reading its URL or title.
        }
        let normalized = normalized.into_string();
        let candidate = *self.fact.as_ref().ok_or(Error::InvalidSource)?.candidate();
        // Admit/allocate the complete definition only after a label match. An
        // oversized first definition cannot be skipped in favor of a later short one.
        let value_len = candidate
            .span_end
            .checked_sub(candidate.span_start)
            .ok_or(Error::InvalidSource)?;
        if value_len > TABLE_WINDOW_MAX_REFERENCE_BYTES {
            return Err(Error::ReferenceLimit);
        }
        self.buffer = None; // Drop label/raw/prefix capacity before allocating the next complete source buffer.
        self.buffer = Some(Buffer::new(
            candidate.span_start..candidate.span_end,
            TABLE_WINDOW_MAX_REFERENCE_BYTES,
        )?);
        self.normalized = Some(normalized);
        self.phase = Phase::CopyDefinition;
        Ok(())
    }

    fn strip(&mut self, available: usize, work: &mut ReferenceWork) -> Result<(), Error> {
        let buffer = self.buffer.as_ref().ok_or(Error::InvalidSource)?;
        let value = self
            .stripped
            .get_or_insert_with(|| String::with_capacity(buffer.text.len()));
        let mut used = 0;
        while self.strip_at < buffer.text.len() && used + 8 <= available {
            if let Some(record) = buffer.prefixes.get(self.strip_record)
                && record.at == buffer.range.start + self.strip_at
            {
                // Remove only prefixes proved by ContainerOnly. Leave additional
                // indentation to the original Parser's label/title semantics.
                self.strip_at += record.bytes;
                self.strip_record += 1;
                used += 1;
                continue;
            }
            let next_prefix = buffer
                .prefixes
                .get(self.strip_record)
                .map_or(buffer.text.len(), |record| record.at - buffer.range.start);
            let mut end = (self.strip_at + (available - used) / 2).min(next_prefix);
            while !buffer.text.is_char_boundary(end) {
                end -= 1; // Retreat at most three UTF-8 tail bytes, retaining the complete scalar for a later step.
                used += 1;
            }
            if end <= self.strip_at {
                return Err(Error::InvalidSource); // Invalid record order/ranges must not cause a zero-progress copy loop.
            }
            value.push_str(&buffer.text[self.strip_at..end]);
            work.copied_bytes += end - self.strip_at;
            used += (end - self.strip_at) * 2;
            self.strip_at = end;
        }
        work.structural_transitions += used;
        if self.strip_at == buffer.text.len() {
            self.buffer = None; // Before Parser decode, release raw candidate/prefix storage and keep only the bounded stripped value.
            self.phase = Phase::DecodeDefinition;
        }
        Ok(())
    }

    fn release_payload(&mut self) {
        // No RefCell or Source borrow crosses this cleanup. Failure/completion
        // synchronously drops all intermediate text and retained scope values.
        self.probe = None;
        self.buffer = None;
        self.normalized = None;
        self.stripped = None;
        self.fact = None;
        self.pending_line = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MarkdownTextSelection,
        markdown_viewport_presentation::parser::markdown_presentation_options,
    };
    use pulldown_cmark::{
        Parser,
        global_frontier::{
            GlobalFact, GlobalFrontier, GlobalFrontierLimits, GlobalStep, StructuralSpineLimits,
        },
    };

    fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        // Every case owns its actual isolated input file and production document;
        // no alternate Source implementation supplies the parser's facts.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("有限引用.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    fn facts(document: &MarkdownDocumentBuffer) -> Vec<VerifiedGlobalReference> {
        // Collect facts only for these finite fixtures from the actual global
        // producer. Its Source frames and opaque scopes remain production-owned;
        // a test-built candidate cannot substitute for verified global evidence.
        let mut global = GlobalFrontier::new(
            document.generation(),
            document.byte_len(),
            markdown_presentation_options(true),
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: 200_000_000,
                },
                max_byte_work: document.byte_len().saturating_mul(1000) + 1_000_000,
                max_transitions: document.byte_len().saturating_mul(10000) + 1_000_000,
            },
        )
        .unwrap();
        let mut result = Vec::new();
        for _ in 0..document.byte_len().saturating_mul(100) + 10000 {
            let frame = document
                .table_source_frame(global.next_byte(), 113)
                .unwrap();
            match global
                .advance(
                    SourceFrame {
                        source_revision: frame.generation,
                        byte_start: frame.byte_start,
                        bytes: &frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    GlobalSourceAuthority {
                        current_revision: document.generation(),
                        cancelled: false,
                    },
                    4096,
                )
                .unwrap()
            {
                GlobalStep::Fact {
                    fact: GlobalFact::Reference(fact),
                    ..
                } => result.push(fact),
                GlobalStep::Complete { .. } => return result,
                _ => (),
            }
        }
        panic!("真实global producer未完成");
    }

    fn run(
        request: &mut Request,
        document: &MarkdownDocumentBuffer,
        queries: &mut Queries,
    ) -> Result<ReferenceWork, Error> {
        // Drive the actual phases with finite fuel. Public work/input accounting
        // checks the contract without claiming to profile the Parser's internals.
        let mut total = ReferenceWork::default();
        for _ in 0..10000 {
            let step = request.advance(document, queries, 4096)?;
            assert!(step.work.source_bytes <= 4096);
            assert!(step.work.structural_transitions + step.work.source_tree_steps <= 4096);
            assert!(step.work.parser_input_bytes <= TABLE_WINDOW_MAX_REFERENCE_BYTES);
            total.source_bytes += step.work.source_bytes;
            total.copied_bytes += step.work.copied_bytes;
            total.metadata_bytes += step.work.metadata_bytes;
            total.source_tree_steps += step.work.source_tree_steps;
            total.structural_transitions += step.work.structural_transitions;
            total.normalizer_input_bytes += step.work.normalizer_input_bytes;
            total.parser_input_bytes += step.work.parser_input_bytes;
            if step.complete {
                return Ok(total);
            }
        }
        panic!("有限引用未完成");
    }

    #[test]
    fn actual_global_scopes_and_multiline_unicode_labels_match_original_value_parser() {
        // Real quote/list continuation, Unicode case folding, escapes and title
        // decoding must agree with the pinned Parser over each complete short input.
        for (source, label) in [
            (
                "[Straße]: <https://example.test/a\\>b?x=&amp;> '标题 &amp; \\'q\\''\n",
                "STRASSE",
            ),
            (
                "> [Maße\n>  Küche]: /first \"Title\n>  Next &amp;\"\n",
                "MASSE KÜCHE",
            ),
            ("- [r]:\n  /target\n  \"标题\n  第二行\"\n", "r"),
            ("[中]: <>\r\n", "中"),
        ] {
            let (_directory, document) = fixture(source);
            let mut queries = Queries::from_projection(&format!("[x][{label}]"), true).unwrap();
            let mut found = facts(&document);
            assert_eq!(found.len(), 1, "{source:?}");
            let mut request = Request::new(
                document.table_source_identity().unwrap(),
                found.remove(0),
                &queries,
            )
            .unwrap()
            .unwrap();
            let work = run(&mut request, &document, &mut queries).unwrap();
            assert!(queries.is_ready());
            let oracle = Parser::new_ext(source, markdown_presentation_options(true));
            let expected = oracle.reference_definitions().get(label).unwrap();
            assert_eq!(
                queries.resolve(label),
                Some((
                    expected.dest.as_ref(),
                    expected.title.as_deref().unwrap_or("")
                )),
                "{source:?}"
            );
            assert!(work.parser_input_bytes > 0);
            assert!(request.buffer.is_none() && request.probe.is_none() && request.fact.is_none());
        }
    }

    #[test]
    fn giant_unrelated_payload_is_never_copied_and_normalized_miss_does_not_read_value() {
        // Actual megabyte URL payloads are present in the original mapping.
        // Count rejection or normalized mismatch must finish before value copying.
        let source = format!(
            "[unrelated-label]: /{}\n[r]: /{}\n",
            "x".repeat(1024 * 1024),
            "y".repeat(1024 * 1024)
        );
        let (_directory, document) = fixture(&source);
        let mut queries = Queries::from_projection("[z]", true).unwrap();
        let mut found = facts(&document);
        assert_eq!(found.len(), 2);
        assert!(
            Request::new(
                document.table_source_identity().unwrap(),
                found.remove(0),
                &queries
            )
            .unwrap()
            .is_none()
        );
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        let work = run(&mut request, &document, &mut queries).unwrap();
        assert_eq!(work.parser_input_bytes, 0);
        assert!(work.copied_bytes <= 2, "只复制r和closer，不读巨大的无关URL");
        assert!(!queries.is_ready());
    }

    #[test]
    fn earliest_matching_oversize_definition_is_sticky_even_with_later_short_definition() {
        // Source order matters: a real oversized first match permanently fails
        // this query inventory even if a later definition would fit its allowance.
        let source = format!(
            "[r]: /{}\n[r]: /later\n",
            "x".repeat(TABLE_WINDOW_MAX_REFERENCE_BYTES + 1)
        );
        let (_directory, document) = fixture(&source);
        let mut found = facts(&document);
        let mut queries = Queries::from_projection("[r]", true).unwrap();
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            run(&mut request, &document, &mut queries),
            Err(Error::ReferenceLimit)
        );
        assert_eq!(
            request.advance(&document, &mut queries, 4096),
            Err(Error::ReferenceLimit)
        );
        assert!(request.buffer.is_none() && request.fact.is_none());
        assert!(
            Request::new(
                document.table_source_identity().unwrap(),
                found.remove(0),
                &queries
            )
            .unwrap()
            .is_none()
        );
        queries.finish_verified_eof();
        assert!(!queries.is_ready());
        assert!(queries.resolve("r").is_none());
    }

    #[test]
    fn source_identity_rejects_other_document_edit_and_undo_without_keeping_root_alive() {
        // Exercise actual equal-byte documents and canonical edit/Undo, then drop
        // the final document to prove the request's identity does not retain its root.
        let (_directory, mut document) = fixture("[r]: /target\n");
        let mut found = facts(&document);
        let mut queries = Queries::from_projection("[r]", true).unwrap();
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        let (_other_directory, other) = fixture("[r]: /target\n");
        assert_eq!(document.generation(), other.generation());
        assert_eq!(
            request.advance(&other, &mut queries, 4096),
            Err(Error::InvalidSource)
        );
        assert!(request.buffer.is_none());
        let mut queries = Queries::from_projection("[r]", true).unwrap();
        let mut found = facts(&document);
        let mut request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        document
            .insert_utf16(0, "x", MarkdownTextSelection::caret(1))
            .unwrap();
        assert!(document.undo_text_edit().is_some());
        assert_eq!(
            request.advance(&document, &mut queries, 4096),
            Err(Error::InvalidSource)
        );
        let mut found = facts(&document);
        let queries = Queries::from_projection("[r]", true).unwrap();
        let request = Request::new(
            document.table_source_identity().unwrap(),
            found.remove(0),
            &queries,
        )
        .unwrap()
        .unwrap();
        let weak = std::sync::Arc::downgrade(document.root.as_ref().unwrap());
        drop(document);
        assert!(
            weak.upgrade().is_none(),
            "Request只保Weak身份和结构scope，不延长正文/mmap生命周期"
        );
        drop(request);
    }
}
