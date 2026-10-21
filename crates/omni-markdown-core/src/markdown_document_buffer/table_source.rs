//! Lend actual piece-tree bytes to the structural producer without copying a document/table or owning another source graph.

use std::sync::Weak;

use super::*;
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableUnderline, MarkdownTableWindowError,
};

#[cfg(test)]
use crate::markdown_viewport_presentation::table_window::{
    MarkdownTableWindowQuota, references::MarkdownTableReferenceQueries,
};

const MAX_TABLE_SOURCE_FRAME_BYTES: usize = 64 * 1024;

mod inline_code;
pub(super) mod inline_reference;
pub(super) mod inline_stack;
mod underline;
pub(crate) use underline::{MarkdownTableUnderlineRequest, MarkdownTableUnderlineRequestStep};

pub(crate) struct BorrowedMarkdownTableSourceFrame<'a> {
    pub generation: u64,
    pub byte_start: usize,
    pub(super) bytes: PieceBytes<'a>,
    pub real_eof: bool,
}

impl BorrowedMarkdownTableSourceFrame<'_> {
    /// Finish the real lease before publishing a successful step or Ready; failure cannot authorize stale continuation.
    pub(crate) fn finish(self) -> Result<(), MarkdownDocumentBufferError> {
        self.bytes.finish()
    }
}

impl std::fmt::Debug for BorrowedMarkdownTableSourceFrame<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Debug output contains bounded structural coordinates, never borrowed
        // document text or a user's content embedded in a default log message.
        formatter
            .debug_struct("BorrowedMarkdownTableSourceFrame")
            .field("generation", &self.generation)
            .field("byte_start", &self.byte_start)
            .field("byte_count", &self.bytes.len())
            .field("real_eof", &self.real_eof)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct MarkdownTableSourceIdentity {
    // The weak control block preserves original root identity without retaining
    // its document, mapping or edit payload; generation separately revokes undo reuse.
    generation: u64,
    byte_len: usize,
    root: Weak<MarkdownPieceTreeNode>,
}

impl std::fmt::Debug for MarkdownTableSourceIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MarkdownTableSourceIdentity")
            .field("generation", &self.generation)
            .field("byte_len", &self.byte_len)
            .finish_non_exhaustive()
    }
}

impl MarkdownTableSourceIdentity {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Require original allocation identity and generation; undo to the same root cannot revive old state.
    pub(crate) fn is_current(&self, document: &MarkdownDocumentBuffer) -> bool {
        document.original_backing_health().is_ok()
            && self.generation == document.generation
            && self.byte_len == document.byte_len()
            && document
                .root
                .as_ref()
                .is_some_and(|root| Weak::ptr_eq(&self.root, &Arc::downgrade(root)))
    }
}

impl MarkdownDocumentBuffer {
    /// Account for one code-permission owner and bounded input, never a paragraph-sized text buffer.
    pub(crate) fn underline_code_workspace_bytes() -> usize {
        inline_code::workspace_bytes()
    }
    /// Exclude irrelevant definitions by actual scanner counts before copying a bounded complete label.
    /// The global producer still owns block/scope membership; a normalized match cannot establish it.
    #[cfg(test)]
    pub(crate) fn table_reference_candidate_label(
        &self,
        source: &MarkdownTableSourceIdentity,
        candidate: &pulldown_cmark::reference_frontier::ReferenceCandidate,
        queries: &MarkdownTableReferenceQueries,
        continuation_prefix: impl Fn(&[u8]) -> Option<usize>,
    ) -> Result<Option<String>, MarkdownTableWindowError> {
        if !source.is_current(self) || candidate.source_revision != self.generation() {
            // Equal generations in different notes do not share the captured
            // root's authority; compare the actual weak identity as well.
            return Err(MarkdownTableWindowError::InvalidSource);
        }
        if !queries.may_match_ascii_count(candidate.label_ordinary_ascii) {
            // A huge excluded ASCII label needs no body copy, destination/title
            // read or second document-wide label inventory.
            return Ok(None);
        }
        let end = candidate
            .label_end
            .checked_add(1)
            .ok_or(MarkdownTableWindowError::InvalidSource)?;
        let raw = self
            .copy_table_source_range(
                candidate.label_start..end,
                queries.maximum_candidate_label_bytes(),
            )
            .map_err(|_| MarkdownTableWindowError::InvalidSource)?;
        let normalized = candidate
            .normalized_label(&raw, continuation_prefix)
            .map_err(|_| MarkdownTableWindowError::ParserMismatch)?;
        if !queries.needs_definition(&normalized)? {
            // The count filter is only necessary, not sufficient. The original
            // normalization and pinned UniCase comparison decide the actual match.
            return Ok(None);
        }
        Ok(Some(normalized.into_string()))
    }

    /// Copy only an explicitly selected bounded source range, transferring its Vec allocation into String.
    pub(crate) fn copy_table_source_range(
        &self,
        byte_range: Range<usize>,
        maximum_bytes: usize,
    ) -> Result<String, MarkdownDocumentBufferError> {
        let Some(length) = byte_range.end.checked_sub(byte_range.start) else {
            // A reversed range cannot become an empty request through saturating
            // subtraction and later acquire false definition-parser authority.
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        };
        if maximum_bytes > MAX_TABLE_SOURCE_FRAME_BYTES
            || length > maximum_bytes
            || byte_range.end > self.byte_len()
        {
            // Admit this operation's actual text before allocation. Total source
            // size or past scan work cannot authorize an oversized current copy.
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        let mut bytes = Vec::with_capacity(length);
        let mut next = byte_range.start;
        while next < byte_range.end {
            // This explicit range is at most 64 KiB. Piece copies total exactly
            // its length and never scan unrelated pieces or the remaining source tail.
            let frame = self.table_source_frame(next, byte_range.end - next)?;
            if frame.bytes.is_empty() {
                // Missing bytes inside a nonempty range indicate inconsistent
                // source/coordinates; do not spin, retry or return a partial string.
                return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
            }
            bytes.extend_from_slice(&frame.bytes);
            next += frame.bytes.len();
            frame.finish()?;
        }
        String::from_utf8(bytes).map_err(|_| MarkdownDocumentBufferError::RangeOutsideDocument)
    }

    /// Empty sources need no table identity; nonempty identities retain only a weak root, not payload lifetime.
    pub(crate) fn table_source_identity(&self) -> Option<MarkdownTableSourceIdentity> {
        self.root.as_ref().map(|root| MarkdownTableSourceIdentity {
            generation: self.generation,
            byte_len: self.byte_len(),
            root: Arc::downgrade(root),
        })
    }

    /// Borrow the actual logical piece, at most 4 KiB, allowing the structural scanner to resume split UTF-8.
    /// The borrow cannot outlive the document and must end before any reentrant native operation.
    pub(crate) fn table_source_frame(
        &self,
        byte_start: usize,
        maximum_bytes: usize,
    ) -> Result<BorrowedMarkdownTableSourceFrame<'_>, MarkdownDocumentBufferError> {
        self.original_backing_health()?;
        if maximum_bytes == 0 || maximum_bytes > MAX_TABLE_SOURCE_FRAME_BYTES {
            return Err(MarkdownDocumentBufferError::MaterializationLimitExceeded);
        }
        let source_end = self.byte_len();
        if byte_start > source_end {
            return Err(MarkdownDocumentBufferError::RangeOutsideDocument);
        }
        if byte_start == source_end {
            // Only the original actual source end supplies an empty EOF frame;
            // a short physical piece or a caller quota does not establish EOF.
            return Ok(BorrowedMarkdownTableSourceFrame {
                generation: self.generation,
                byte_start,
                bytes: PieceBytes::Edited(&[]),
                real_eof: true,
            });
        }
        let mut node = self
            .root
            .as_deref()
            .ok_or(MarkdownDocumentBufferError::RangeOutsideDocument)?;
        let mut local = byte_start;
        loop {
            match node {
                MarkdownPieceTreeNode::Branch { left, right, .. } => {
                    // Descend by original byte metrics without producing a flat
                    // piece directory or converting byte coordinates through UTF-16.
                    let left_len = left.metrics().byte_len;
                    if local < left_len {
                        node = left.as_ref();
                    } else {
                        local -= left_len;
                        node = right.as_ref();
                    }
                }
                MarkdownPieceTreeNode::OriginalSpan(span) => {
                    // The compact directory locates one original logical piece.
                    // Its requested subrange remains bounded by that physical piece.
                    let hit = span.locate_byte(local, false)?;
                    let end = hit
                        .local
                        .saturating_add(maximum_bytes)
                        .min(hit.piece.byte_len);
                    let bytes = self.piece_range_bytes(&hit.piece, hit.local..end)?;
                    let real_eof = byte_start + bytes.len() == source_end;
                    return Ok(BorrowedMarkdownTableSourceFrame {
                        generation: self.generation,
                        byte_start,
                        bytes,
                        real_eof,
                    });
                }
                MarkdownPieceTreeNode::Leaf(piece) => {
                    let end = local.saturating_add(maximum_bytes).min(piece.byte_len);
                    let bytes = self.piece_range_bytes(piece, local..end)?;
                    // Obtain finite length facts while the real lease is live,
                    // then transfer its unique owner without cloning or early-drop tricks.
                    let real_eof = byte_start + bytes.len() == source_end;
                    return Ok(BorrowedMarkdownTableSourceFrame {
                        generation: self.generation,
                        byte_start,
                        bytes,
                        real_eof,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::block_frontier::SourceFrame;
    use pulldown_cmark::reference_frontier::{
        ReferenceCandidate, ReferenceDefinitionScanner, ReferenceStep,
    };

    fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        // All actual file I/O is confined to this private synthetic fixture;
        // the returned directory owns only the source created by this test.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("S2-owned-source.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        (directory, document)
    }

    /// Replay one genuine Global; the fixture's initial root is itself a legal recoverable boundary.
    fn underline_global(
        document: &MarkdownDocumentBuffer,
    ) -> pulldown_cmark::global_frontier::GlobalFrontier {
        use pulldown_cmark::global_frontier::{
            GlobalFrontier, GlobalFrontierLimits, StructuralSpineLimits,
        };
        GlobalFrontier::new(
            document.generation(),
            document.byte_len(),
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: document.byte_len() * 2048 + 1024 * 1024,
                },
                max_byte_work: document.byte_len() * 256 + 1024 * 1024,
                max_transitions: document.byte_len() * 2048 + 1024 * 1024,
            },
        )
        .unwrap()
    }

    fn reference(document: &MarkdownDocumentBuffer, maximum: usize) -> ReferenceCandidate {
        // Feed actual original pieces to the shared scanner. Finish every lease
        // before inspecting the scanner's candidate or requesting another frame.
        let identity = document.table_source_identity().unwrap();
        let mut scanner =
            ReferenceDefinitionScanner::new(document.generation(), document.byte_len(), 0).unwrap();
        loop {
            assert!(identity.is_current(document));
            let frame = document
                .table_source_frame(scanner.next_byte(), maximum)
                .unwrap();
            let step = scanner
                .advance(
                    SourceFrame {
                        source_revision: frame.generation,
                        byte_start: frame.byte_start,
                        bytes: &frame.bytes,
                        real_eof: frame.real_eof,
                    },
                    maximum,
                )
                .unwrap();
            frame.finish().unwrap();
            match step {
                ReferenceStep::Candidate { candidate, .. } => return candidate,
                ReferenceStep::Progress { consumed } | ReferenceStep::NeedInput { consumed } => {
                    assert!(consumed > 0);
                }
                _ => panic!("本fixture为同一真实行内的完整定义，不需要容器续行或fallback"),
            }
        }
    }

    #[test]
    fn source_frames_borrow_exact_original_and_edited_piece_bytes_at_every_byte() {
        // Compare finite frame bytes at every original/edited offset, including
        // UTF-8 interiors; byte framing must not invent scalar alignment or EOF.
        let original = "中😀e\u{301}\r\nsource".repeat(1000);
        let (_directory, mut document) = fixture(&original);
        document
            .insert_utf16(1, "edited😀", MarkdownTextSelection::caret(9))
            .unwrap();
        let expected = format!("中edited😀{}", &original["中".len()..]);
        for start in 0..=expected.len() {
            let frame = document.table_source_frame(start, 3).unwrap();
            assert_eq!(
                &*frame.bytes,
                &expected.as_bytes()[start..start + frame.bytes.len()]
            );
            assert!(frame.bytes.len() <= 3);
            assert_eq!(frame.real_eof, start + frame.bytes.len() == expected.len());
            frame.finish().unwrap();
        }
    }

    #[test]
    fn reference_source_adapter_uses_real_piece_leaves_and_preserves_edit_undo_redo_bytes() {
        // Vary actual frame length across a real definition, then exercise edits
        // and undo/redo to prove that old generation authority cannot revive.
        let source = "[r]: <https://example.test/😀> \"Title中\"\r\nrest\n";
        let (_directory, mut document) = fixture(source);
        for maximum in 1..=source.len() {
            let candidate = reference(&document, maximum);
            assert_eq!(
                &source[candidate.destination_start..candidate.destination_end],
                "https://example.test/😀"
            );
            assert_eq!(candidate.span_end, source.find("\r\n").unwrap());
        }
        let old = document.table_source_identity().unwrap();
        document
            .replace_utf16_range(1..2, "new", MarkdownTextSelection::caret(4))
            .unwrap();
        assert!(!old.is_current(&document));
        assert_eq!(reference(&document, 3).label_end, 4);
        document.undo().unwrap();
        assert!(
            !old.is_current(&document),
            "同root的Undo仍推进generation，旧cursor不能自动复活"
        );
        assert_eq!(reference(&document, 3).label_end, 2);
        document.redo().unwrap();
        assert_eq!(reference(&document, 3).label_end, 4);
    }

    #[test]
    fn finite_reference_label_copy_uses_piece_source_and_rejects_stale_root() {
        // Real normalization accepts the finite Unicode label. Equal generation
        // in another note and later edits must both revoke the original identity.
        let (_directory, mut document) = fixture("[Maße]: <https://x.test/中> '标题'\n");
        let identity = document.table_source_identity().unwrap();
        let candidate = reference(&document, 1);
        let queries = MarkdownTableReferenceQueries::from_projection("[x][MASSE]", true).unwrap();
        assert_eq!(
            document
                .table_reference_candidate_label(&identity, &candidate, &queries, |_| Some(0))
                .unwrap(),
            Some("Maße".to_owned())
        );
        let (_other_directory, other) = fixture("[Maße]: /other\n");
        assert_eq!(
            other.table_reference_candidate_label(&identity, &candidate, &queries, |_| Some(0)),
            Err(MarkdownTableWindowError::InvalidSource)
        );
        document
            .insert_utf16(0, "changed", MarkdownTextSelection::caret(7))
            .unwrap();
        assert_eq!(
            document.table_reference_candidate_label(&identity, &candidate, &queries, |_| Some(0)),
            Err(MarkdownTableWindowError::InvalidSource)
        );
    }

    #[test]
    fn source_range_copy_is_bounded_and_rejects_partial_utf8() {
        // Explicit copy retains complete validated UTF-8 and rejects oversized,
        // reversed, beyond-EOF and partial-scalar ranges without truncating output.
        let (_directory, document) = fixture("中😀tail");
        assert_eq!(document.copy_table_source_range(0..7, 7).unwrap(), "中😀");
        assert!(document.copy_table_source_range(0..7, 6).is_err());
        assert!(document.copy_table_source_range(1..3, 2).is_err());
        let invalid = Range { start: 4, end: 3 };
        assert!(document.copy_table_source_range(invalid, 16).is_err());
        assert!(document.copy_table_source_range(0..20, 20).is_err());
    }

    #[test]
    fn same_generation_different_document_and_accepted_drop_cannot_reuse_source_identity() {
        // Keep only the weak identity while retiring its actual source owner;
        // another equal-text document must remain outside that original permission.
        let (_first_dir, first) = fixture("same bytes");
        let (_second_dir, second) = fixture("same bytes");
        let identity = first.table_source_identity().unwrap();
        assert!(!identity.is_current(&second));
        assert!(identity.root.strong_count() > 0);
        drop(first);
        assert_eq!(
            identity.root.strong_count(),
            0,
            "源身份不能强持原root或document payload"
        );
        let (_empty_dir, empty) = fixture("");
        assert!(empty.table_source_identity().is_none());
        assert!(empty.table_source_frame(0, 1).unwrap().real_eof);
    }

    #[test]
    fn real_source_context_maps_byte_pairs_to_utf16_without_reading_huge_content_as_string() {
        // Use a huge real source with a small viewport coverage. Incremental
        // pairing must derive original UTF-16 values and reject a later source edit.
        let mut source = String::from("😀<u>");
        source.extend(std::iter::repeat_n('中', 400_000));
        source.push_str("</u>tail");
        let (_directory, mut document) = fixture(&source);
        let coverage = 4..10;
        let mut request = document
            .table_underline_request(
                std::slice::from_ref(&coverage),
                MarkdownTableWindowQuota::new(8192, 1024 * 1024),
            )
            .unwrap();
        let mut global = underline_global(&document);
        while request.ready_pairs_for(&document).unwrap().is_none() {
            let step = request.advance(&document, &mut global, 65536).unwrap();
            assert!(
                step.ready
                    || step.source_bytes > 0
                    || step.utf8_validation_bytes > 0
                    || step.structural_transitions > 0
            );
            assert!(step.source_bytes <= 4096);
            assert!(step.utf8_validation_bytes <= step.source_bytes * 4);
        }
        assert_eq!(
            request.ready_pairs_for(&document).unwrap().unwrap(),
            &[MarkdownTableUnderline {
                open: 2..5,
                content: 5..400_005,
                close: 400_005..400_009
            }]
        );
        assert!(
            request.context.is_none(),
            "最终UTF16值就绪后不保byte中间集合"
        );
        document
            .insert_utf16(0, "x", MarkdownTextSelection::caret(1))
            .unwrap();
        assert_eq!(
            request.ready_pairs_for(&document),
            Err(MarkdownTableWindowError::InvalidSource)
        );
        assert_eq!(
            request.advance(&document, &mut global, 1),
            Err(MarkdownTableRequestError::SourceSuperseded)
        );
    }

    #[test]
    fn dense_tags_in_large_edit_leaf_do_not_trigger_per_pair_prefix_decoding() {
        let (_directory, mut document) = fixture("seed");
        // One actual edit creates a large edit leaf. Repeated 64-pair batches
        // must not decode that leaf's long prefix again for every batch.
        let source = format!("{}{}", "中".repeat(18_000), "<u>😀</u>".repeat(128));
        document
            .replace_utf16_range(0..4, &source, MarkdownTextSelection::caret(0))
            .unwrap();
        let coverage = 0..document.byte_len();
        let mut request = document
            .table_underline_request(
                std::slice::from_ref(&coverage),
                MarkdownTableWindowQuota::new(8192, 1024 * 1024),
            )
            .unwrap();
        let mut reads = 0;
        let mut validations = 0;
        let mut tree_steps = 0;
        let mut global = underline_global(&document);
        loop {
            let step = request.advance(&document, &mut global, 65536).unwrap();
            reads += step.source_bytes;
            validations += step.utf8_validation_bytes;
            tree_steps += step.source_tree_steps;
            assert!(step.source_bytes <= 4096);
            assert!(step.utf8_validation_bytes <= 4 * step.source_bytes);
            if step.ready {
                break;
            }
        }
        // Reported source traversal stays linear; UTF-8 validation remains a
        // constant multiple rather than restarting the prefix for every pair.
        assert_eq!(reads, source.len());
        assert!(validations <= source.len() * 4);
        assert!(
            tree_steps
                <= (source.len().div_ceil(4096) + 2)
                    * usize::from(document.root.as_ref().unwrap().height())
        );
        assert_eq!(
            request.ready_pairs_for(&document).unwrap().unwrap().len(),
            128
        );
    }

    #[test]
    fn no_html_viewport_keeps_the_original_global_untouched_and_reads_only_its_prefix() {
        // A viewport with no candidate HTML needs only its actual prefix, without
        // replaying Global classification over the huge remaining plain source.
        let (_directory, document) = fixture(&"中😀普通正文\n".repeat(40_000));
        let mut global = underline_global(&document);
        let initial = global.total_work();
        let coverage = 0..128;
        let mut request = document
            .table_underline_request(
                std::slice::from_ref(&coverage),
                MarkdownTableWindowQuota::new(8192, 1024 * 1024),
            )
            .unwrap();
        let mut reads = 0;
        while request.ready_pairs_for(&document).unwrap().is_none() {
            let step = request.advance(&document, &mut global, 4096).unwrap();
            reads += step.source_bytes;
            assert_eq!(step.global_source_bytes, 0);
            assert_eq!(step.global_source_tree_steps, 0);
            assert!(step.source_bytes + step.structural_transitions <= 4096);
        }
        // Account through the real scalar end near the requested endpoint. This
        // fast path neither replays the huge block nor decodes a whole-source String.
        assert!(reads >= coverage.end && reads <= coverage.end + 3);
        assert_eq!(global.total_work(), initial);
        assert_eq!(global.next_byte(), 0);
        assert!(!global.semantic_boundary_retained());
        assert_eq!(
            request.ready_pairs_for(&document).unwrap(),
            Some([].as_slice())
        );
    }
}
