//! Bounded UTF-16 font-run scratch for one active storage, without ownership of the canonical document.

use super::cache_owner::CachePayload;
use super::{
    NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS, SharedMarkdownDocument, WeakMarkdownDocument,
};
use std::ops::Range;

/// When the main scratch is loaned, copy exactly one complete query scalar into two stack units.
pub(super) fn copy_query_scalar(
    document: &SharedMarkdownDocument,
    range: &Range<usize>,
    output: &mut [u16; 2],
) -> Option<usize> {
    let source = document.borrow();
    let count = range.end.checked_sub(range.start)?;
    if count == 0
        || count > output.len()
        || source.byte_offset_for_utf16(range.start).is_err()
        || source.byte_offset_for_utf16(range.end).is_err()
        || source
            .copy_utf16_code_units(range.start, &mut output[..count])
            .is_err()
    {
        // Reject split surrogates, excess units, or failed copies without publishing partial text or a
        // substitute base font.
        return None;
    }
    let mut decoded = char::decode_utf16(output[..count].iter().copied());
    if decoded.next()?.is_err() || decoded.next().is_some() {
        // Two BMP characters are not one scalar; accept one BMP scalar or one complete supplementary
        // pair only.
        return None;
    }
    Some(count)
}

/// One active buffer whose key certifies text only; each query still recomputes its actual style and
/// font policy.
pub(super) struct FontRunScratch {
    identity: WeakMarkdownDocument,
    generation: Option<u64>,
    coverage: Range<usize>,
    pub(super) units: Vec<u16>,
}

impl CachePayload for FontRunScratch {
    // Closed/empty slots carry no text capacity or document identity.
    fn empty() -> Self {
        Self {
            identity: WeakMarkdownDocument::new(),
            generation: None,
            coverage: 0..0,
            units: Vec::new(),
        }
    }

    fn invalidate(&mut self) {
        // Editing revokes the key while the active session retains its bounded capacity for the next
        // query.
        self.identity = WeakMarkdownDocument::new();
        self.generation = None;
        self.coverage = 0..0;
        self.units.clear();
    }
}

impl FontRunScratch {
    /// An explicitly active storage reserves one bounded buffer; an idle plugin owns no such scratch.
    pub(super) fn new() -> Self {
        // Reserve the fixed window only when this active storage is constructed, never for every font
        // query.
        Self {
            units: Vec::with_capacity(NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS),
            ..Self::empty()
        }
    }

    /// A hit must cover the complete scalar; return the intersection of the current semantic/request
    /// range and cached text.
    pub(super) fn prepare(
        &mut self,
        document: &SharedMarkdownDocument,
        style_range: Range<usize>,
        scalar_range: Range<usize>,
    ) -> Option<Range<usize>> {
        self.prepare_with(
            document,
            style_range,
            scalar_range,
            |document, start, units| {
                // Copy actual piece-tree contents only on a miss; existing tests observe this boundary
                // without changing layout.
                document.copy_utf16_code_units(start, units).is_ok()
            },
        )
    }

    /// Validate scalar boundaries before changing the key; install new coverage only after the complete
    /// bounded copy succeeds.
    fn prepare_with(
        &mut self,
        document: &SharedMarkdownDocument,
        style_range: Range<usize>,
        scalar_range: Range<usize>,
        copy: impl FnOnce(&omni_markdown_core::MarkdownDocumentBuffer, usize, &mut [u16]) -> bool,
    ) -> Option<Range<usize>> {
        let source = document.borrow();
        if style_range.start > scalar_range.start
            || scalar_range.start >= scalar_range.end
            || scalar_range.end > style_range.end
            || style_range.end > source.utf16_len()
            || style_range.len() > NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS
            || source.byte_offset_for_utf16(style_range.start).is_err()
            || source.byte_offset_for_utf16(style_range.end).is_err()
            || source.byte_offset_for_utf16(scalar_range.start).is_err()
            || source.byte_offset_for_utf16(scalar_range.end).is_err()
        {
            // Invalid requests preserve the existing valid buffer and cannot treat half a surrogate as
            // a hit.
            return None;
        }
        // Generation alone is insufficient: distinct live documents may share the same generation.
        let identity = std::rc::Rc::downgrade(document);
        let generation = source.generation();
        let hit = std::rc::Weak::ptr_eq(&self.identity, &identity)
            && self.generation == Some(generation)
            && self.coverage.start <= scalar_range.start
            && scalar_range.end <= self.coverage.end;
        if !hit {
            // A miss copies the requested bounded style range only, without extending prefetch beyond
            // that range.
            self.generation = None;
            self.coverage = 0..0;
            self.identity = WeakMarkdownDocument::new();
            if self.units.capacity() < NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS {
                // After an invalid loan returns an empty slot, reserve the fixed limit exactly instead
                // of doubling beyond 4096 units.
                self.units
                    .reserve_exact(NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS - self.units.len());
            }
            self.units.resize(style_range.len(), 0);
            if !copy(&source, style_range.start, &mut self.units) {
                // A failed partial write retains no key or coverage, so a later query must refill
                // instead of reading it as valid text.
                self.units.clear();
                return None;
            }
            self.identity = identity;
            self.generation = Some(generation);
            self.coverage = style_range.clone();
        }
        // Current semantics may be narrower than the old buffer; scan only their intersection, never
        // the previous style range.
        Some(style_range.start.max(self.coverage.start)..style_range.end.min(self.coverage.end))
    }

    /// Borrow only the intersection returned by prepare; the caller must retain its local loan
    /// throughout font scanning.
    pub(super) fn slice(&self, range: &Range<usize>) -> &[u16] {
        &self.units[range.start - self.coverage.start..range.end - self.coverage.start]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_markdown_core::MarkdownDocumentBuffer;
    use std::cell::{Cell, RefCell};
    use std::io::Write;
    use std::rc::Rc;

    /// Open a real private snapshot/piece tree; these tests construct no AppKit objects.
    fn fixture(text: &str) -> (tempfile::NamedTempFile, SharedMarkdownDocument) {
        let mut file = tempfile::NamedTempFile::new().expect("synthetic source");
        file.write_all(text.as_bytes()).expect("write source");
        let document = MarkdownDocumentBuffer::open(file.path(), 64 * 1024).expect("document");
        (file, Rc::new(RefCell::new(document)))
    }

    #[test]
    fn old_copy_policy_counts_actual_piece_tree_reads() {
        // Keep the previous policy as a baseline: each query resizes and copies the same range from the
        // actual piece tree.
        let mut file = tempfile::NamedTempFile::new().expect("synthetic source");
        file.write_all(&vec![b'a'; 32 * 1024])
            .expect("write source");
        let document = MarkdownDocumentBuffer::open(file.path(), 64 * 1024).expect("document");
        let mut scratch = Vec::with_capacity(4096);
        let mut copies = 0;
        let mut copied_units = 0;
        for _ in 0..1000 {
            // Count actual copy operations here; resizing alone does not prove an allocation occurred.
            scratch.resize(4096, 0);
            document
                .copy_utf16_code_units(6145, &mut scratch)
                .expect("bounded copy");
            copies += 1;
            copied_units += scratch.len();
            assert!(scratch.iter().all(|unit| *unit == u16::from(b'a')));
        }
        assert_eq!(copies, 1000);
        assert_eq!(copied_units, 4_096_000);
        eprintln!(
            "old_utf16_copy_baseline calls={copies} units={copied_units} bytes={}",
            copied_units * 2
        );
    }

    #[test]
    fn repeated_and_shifted_queries_reuse_real_piece_tree_content() {
        let (_file, document) = fixture(&"a".repeat(32 * 1024));
        let mut scratch = FontRunScratch::new();
        let copies = Cell::new(0);
        let units = Cell::new(0);
        for _ in 0..1000 {
            // Use the same real source, range, and queries as the baseline, counting at the actual copy
            // boundary.
            let range = scratch
                .prepare_with(
                    &document,
                    6145..10241,
                    8192..8193,
                    |source, start, target| {
                        copies.set(copies.get() + 1);
                        units.set(units.get() + target.len());
                        source.copy_utf16_code_units(start, target).is_ok()
                    },
                )
                .expect("current range");
            assert_eq!(range, 6145..10241);
            assert!(
                scratch
                    .slice(&range)
                    .iter()
                    .all(|unit| *unit == u16::from(b'a'))
            );
        }
        // A shifted centered request can reuse the same text and return the valid, narrower
        // intersection.
        let range = scratch
            .prepare_with(&document, 6146..10242, 8193..8194, |_, _, _| {
                panic!("coverage hit must not copy")
            })
            .expect("shifted hit");
        assert_eq!(range, 6146..10241);
        assert_eq!(copies.get(), 1);
        assert_eq!(units.get(), 4096);
        eprintln!(
            "new_utf16_copy_counts calls={} units={} bytes={}",
            copies.get(),
            units.get(),
            units.get() * 2
        );
    }

    #[test]
    fn semantic_intersection_and_low_surrogate_keep_true_font_boundaries() {
        let (_file, document) = fixture("A😀中B");
        let mut scratch = FontRunScratch::new();
        assert_eq!(scratch.prepare(&document, 0..5, 1..3), Some(0..5));
        let range = scratch
            .prepare_with(&document, 1..4, 1..3, |_, _, _| panic!("hit"))
            .expect("semantic clip");
        // Run the production font-run algorithm with the query starting at the emoji low surrogate.
        let run = super::super::text_storage::native_markdown_font_run_range_by(
            scratch.slice(&range),
            range.start,
            1,
            |scalar| !scalar.is_ascii(),
        );
        assert_eq!(run, 1..4);
        let range = scratch
            .prepare(&document, 1..3, 1..3)
            .expect("narrower semantic style");
        let run = super::super::text_storage::native_markdown_font_run_range_by(
            scratch.slice(&range),
            range.start,
            1,
            |scalar| !scalar.is_ascii(),
        );
        assert_eq!(run, 1..3);
        // A low-surrogate-only range is invalid; containing one queried index does not prove complete
        // scalar coverage.
        assert!(scratch.prepare(&document, 2..3, 1..3).is_none());
    }

    #[test]
    fn same_generation_document_aba_and_edit_history_force_fresh_copy() {
        let (_first, first) = fixture("AAAA");
        let (_second, second) = fixture("BBBB");
        let mut scratch = FontRunScratch::new();
        scratch.prepare(&first, 0..4, 0..1).expect("first");
        assert_eq!(first.borrow().generation(), second.borrow().generation());
        scratch
            .prepare(&second, 0..4, 0..1)
            .expect("different identity");
        assert_eq!(scratch.slice(&(0..4)), &vec![u16::from(b'B'); 4]);
        second
            .borrow_mut()
            .replace_utf16_range(
                0..1,
                "C",
                omni_markdown_core::MarkdownTextSelection::caret(1),
            )
            .expect("edit");
        scratch.prepare(&second, 0..4, 0..1).expect("edited");
        assert_eq!(scratch.slice(&(0..4))[0], u16::from(b'C'));
        second.borrow_mut().undo().expect("undo");
        scratch
            .prepare(&second, 0..4, 0..1)
            .expect("undo changed generation");
        assert_eq!(scratch.slice(&(0..4))[0], u16::from(b'B'));
        second.borrow_mut().redo().expect("redo");
        scratch
            .prepare(&second, 0..4, 0..1)
            .expect("redo changed generation");
        assert_eq!(scratch.slice(&(0..4))[0], u16::from(b'C'));
    }

    #[test]
    fn invalid_ranges_preserve_cache_and_failed_copy_never_publishes_key() {
        let (_file, document) = fixture(&"a".repeat(5000));
        let mut scratch = FontRunScratch::new();
        scratch.prepare(&document, 0..4096, 0..1).expect("valid");
        assert!(
            scratch
                .prepare_with(&document, 0..4097, 0..1, |_, _, _| panic!(
                    "oversize cannot copy"
                ))
                .is_none()
        );
        assert_eq!(
            scratch.prepare_with(&document, 0..4096, 0..1, |_, _, _| panic!(
                "old hit preserved"
            )),
            Some(0..4096)
        );
        assert!(
            scratch
                .prepare_with(&document, 4096..5000, 4096..4097, |_, _, output| {
                    output.fill(7);
                    false
                })
                .is_none()
        );
        assert!(scratch.generation.is_none());
        assert_eq!(scratch.coverage, 0..0);
        assert_eq!(scratch.identity.strong_count(), 0);
        let calls = Cell::new(0);
        scratch
            .prepare_with(
                &document,
                4096..5000,
                4096..4097,
                |source, start, output| {
                    // The same query after failure must read the source again instead of reusing a
                    // partial write.
                    calls.set(calls.get() + 1);
                    source.copy_utf16_code_units(start, output).is_ok()
                },
            )
            .expect("refill");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn ready_invalidation_reuses_capacity_but_close_drops_all_keys_and_capacity() {
        use super::super::cache_owner::{CacheAdmission, CacheOwner};
        let (_file, document) = fixture("A😀B");
        let owner = CacheOwner::new(FontRunScratch::new());
        let address = {
            let CacheAdmission::Ready(mut lease) = owner.begin() else {
                panic!("ready")
            };
            lease
                .payload_mut()
                .prepare(&document, 0..4, 1..3)
                .expect("prepare");
            lease.payload_mut().units.as_ptr()
        };
        owner.invalidate();
        let CacheAdmission::Ready(mut lease) = owner.begin() else {
            panic!("ready after invalidation")
        };
        assert_eq!(lease.payload_mut().units.as_ptr(), address);
        assert!(lease.payload_mut().generation.is_none());
        assert!(lease.payload_mut().units.is_empty());
        assert_eq!(lease.payload_mut().identity.strong_count(), 0);
        // Closing must prevent the loaned buffer from returning to service; weak observations must not
        // retain the document.
        owner.close();
        assert!(!owner.is_released());
        drop(lease);
        assert!(owner.is_released());
        assert!(matches!(owner.begin(), CacheAdmission::Closed));
    }

    #[test]
    fn loaned_query_uses_exact_supplementary_scalar_with_only_two_stack_units() {
        use super::super::cache_owner::{CacheAdmission, CacheOwner};
        let (_file, document) = fixture("A😀中B");
        let owner = CacheOwner::new(FontRunScratch::new());
        let CacheAdmission::Ready(outer) = owner.begin() else {
            panic!("outer")
        };
        assert!(matches!(owner.begin(), CacheAdmission::Loaned));
        let mut output = [0_u16; 2];
        assert_eq!(copy_query_scalar(&document, &(1..3), &mut output), Some(2));
        // A low-surrogate query at local_index=1 still resolves the same complete emoji, without a
        // replacement scalar.
        let scalar = super::super::text_storage::native_markdown_utf16_scalar_at(&output, 1)
            .expect("scalar");
        assert_eq!(scalar.value, '😀');
        assert_eq!(copy_query_scalar(&document, &(2..3), &mut output), None);
        assert_eq!(copy_query_scalar(&document, &(3..5), &mut output), None);
        drop(outer);
    }

    #[test]
    fn empty_or_eof_queries_do_not_copy_and_refill_capacity_never_doubles_past_limit() {
        let (_empty_file, empty) = fixture("");
        let mut scratch = FontRunScratch::empty();
        assert!(
            scratch
                .prepare_with(&empty, 0..0, 0..0, |_, _, _| panic!("empty cannot copy"))
                .is_none()
        );
        assert_eq!(scratch.units.capacity(), 0);
        let (_file, document) = fixture(&"x".repeat(5000));
        assert!(
            scratch
                .prepare_with(&document, 5000..5000, 5000..5001, |_, _, _| panic!(
                    "EOF cannot copy"
                ))
                .is_none()
        );
        // Grow requests from an empty slot without allowing ordinary Vec growth to double capacity to
        // 6000 units.
        scratch
            .prepare(&document, 0..3000, 0..1)
            .expect("small fill");
        scratch
            .prepare(&document, 0..4096, 3500..3501)
            .expect("large fill");
        assert_eq!(
            scratch.units.capacity(),
            NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS
        );
    }
}
