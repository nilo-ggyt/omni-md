//! Read-only aggregate metrics and bounded-memory literal search over document pieces.

use super::*;

impl MarkdownDocumentBuffer {
    /// Read raw HTML-introducer risk in O(1), without a per-caret prefix scan or a separate syntax index.
    pub(crate) fn contains_ascii_less_than(&self) -> bool {
        // The leaf/node OR summary follows canonical edits and structural undo/redo automatically.
        piece_tree_metrics(&self.root).contains_ascii_less_than
    }

    /// Return Unicode scalar count, treating CRLF as one CodeMirror-compatible character.
    ///
    /// Piece-tree aggregates make this read-only query O(1), including unsaved edits.
    #[must_use]
    pub fn character_count(&self) -> usize {
        // The root already joins split CRLF boundaries; no document mirror or scan is needed.
        piece_tree_metrics(&self.root).character_count
    }

    /// Count the preserved editor's nonempty Unicode 17 letter/number runs (`[\p{L}\p{N}]+`).
    ///
    /// Combining marks, punctuation, emoji, controls, and separators break runs; no normalization or
    /// locale segmentation is applied. This O(1) aggregate includes live edits and merges L/N runs
    /// split across piece boundaries, matching the original regexp applied to each CodeMirror line.
    #[must_use]
    pub fn word_count(&self) -> usize {
        // Reading the aggregate never creates a retained token list or background index.
        piece_tree_metrics(&self.root).word_count
    }

    /// Find a literal query across pieces without flattening the document.
    ///
    /// Forward search selects the first match starting at or after `start`; backward
    /// search selects the last match ending at or before `start`. The caller owns
    /// wraparound. Empty queries return `None`. Case-insensitive mode folds ASCII
    /// only and compares non-ASCII literally, preserving Unicode scalar coordinates.
    /// Query-sized KMP tables bound extra memory independently from document size.
    ///
    /// # Errors
    ///
    /// Reject an out-of-document start or a UTF-16 offset inside a surrogate pair.
    pub fn find_utf16(
        &self,
        query: &str,
        start: usize,
        backwards: bool,
        case_sensitive: bool,
    ) -> Result<Option<Range<usize>>, MarkdownDocumentBufferError> {
        // Validate before constructing the pattern so rejected positions cannot trigger a scan.
        let byte_start = self.byte_offset_for_utf16(start)?;
        if query.is_empty() || query.len() > self.byte_len() {
            // No literal match exists for an empty/oversized pattern; document state stays intact.
            return Ok(None);
        }

        // Preserve UTF-16 length during ASCII folding so reported ranges remain original ranges.
        let fold = |unit: u16| {
            // Fold only width-preserving ASCII code units so match ranges stay in the original UTF-16 coordinate space.
            if !case_sensitive && (b'A' as u16..=b'Z' as u16).contains(&unit) {
                // ASCII case pairs have identical width and cannot split a Unicode scalar.
                unit + u16::from(b'a' - b'A')
            } else {
                // Non-ASCII code units remain literal; no expanding Unicode fold is attempted.
                unit
            }
        };
        let pattern = query.encode_utf16().map(fold).collect::<Vec<_>>();
        let mut prefix_lengths = vec![0; pattern.len()];
        let mut prefix = 0;
        for index in 1..pattern.len() {
            // Reuse shorter borders until this code unit can extend the existing query prefix.
            while prefix > 0 && pattern[index] != pattern[prefix] {
                prefix = prefix_lengths[prefix - 1];
            }
            if pattern[index] == pattern[prefix] {
                // Record the longest proper border for linear-time overlapping-match fallback.
                prefix += 1;
                prefix_lengths[index] = prefix;
            }
        }

        // Restrict the stream to the caller's direction; wrapping is an explicit UI operation.
        let (range, mut position) = if backwards {
            // Prefix scanning retains only the latest complete match before the caret.
            (0..byte_start, 0)
        } else {
            // Suffix scanning cannot accidentally return a match that starts before the caret.
            (byte_start..self.byte_len(), start)
        };
        let mut matched = 0;
        let mut found = None;
        visit_piece_tree_range(self, self.root.as_deref(), range, 0, &mut |bytes| {
            // Carry query-sized KMP state across immutable piece leaves without flattening the document.
            if found.is_some() && !backwards {
                // A forward result is final; remaining leaves require no decoding or text scan.
                return Ok(());
            }
            // Every leaf/range boundary is scalar-aligned by the document's edit invariant.
            let text = std::str::from_utf8(bytes).map_err(|_| {
                // Invalid source encoding must remain an explicit failure rather than silently replacing bytes.
                MarkdownDocumentBufferError::InvalidUtf8Document
            })?;
            for unit in text.encode_utf16() {
                // Prefix state spans leaves, so a match may cross original/edit segment joins.
                let unit = fold(unit);
                // Reuse the longest remaining query border after a mismatch, keeping total scan work linear.
                while matched > 0 && unit != pattern[matched] {
                    matched = prefix_lengths[matched - 1];
                }
                if unit == pattern[matched] {
                    // Advance only a matching prefix; valid query UTF-16 cannot end mid-scalar.
                    matched += 1;
                }
                position += 1;
                if matched == pattern.len() {
                    // Keep either the first suffix match or the latest prefix match, as requested.
                    found = Some(position - pattern.len()..position);
                    if !backwards {
                        // The forward result requires no further work within the current leaf.
                        break;
                    }
                    // Backward search still accepts overlaps while retaining only one range.
                    matched = prefix_lengths[matched - 1];
                }
            }
            Ok(())
        })?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    //! Live-edit metrics and literal-search behavior over original/edit piece boundaries.

    use super::*;

    /// Load the opt-in Node/CodeMirror-generated oracle without requiring JavaScript in Rust test runs.
    fn word_count_reference() -> serde_json::Value {
        // Reject an accidental fixture schema/version/category change before interpreting any digest.
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("queries/word_count_reference.json"))
                .expect("frozen original word-count reference");
        assert_eq!(reference["schema"], 1);
        assert_eq!(reference["provenance"]["node"], "24.21.0");
        assert_eq!(reference["provenance"]["unicode"], "17.0");
        assert_eq!(reference["provenance"]["regexp"], r"/[\p{L}\p{N}]+/gu");
        assert_eq!(
            reference["provenance"]["category"],
            "General_Category=Letter|Number"
        );
        assert_eq!(unicode_properties::UNICODE_VERSION, (17, 0, 0));
        reference
    }

    /// Keep synthetic source paths and their source-independent document snapshot isolated per test.
    struct Fixture {
        document: MarkdownDocumentBuffer,
        _directory: tempfile::TempDir,
    }

    impl Fixture {
        /// Create an isolated synthetic UTF-8 source without accessing user notes.
        fn new(text: &str) -> Self {
            // TempDir scopes disposable input paths; the document owns its distinct immutable snapshot.
            let directory = tempfile::tempdir().expect("temporary document directory");
            let path = directory.path().join("Note.md");
            std::fs::write(&path, text).expect("write source fixture");
            let document = MarkdownDocumentBuffer::open(&path, 2 * 1_024 * 1_024)
                .expect("open source fixture");
            Self {
                document,
                _directory: directory,
            }
        }
    }

    /// Aggregates merge words and CRLF correctly after structural edits and undo/redo.
    #[test]
    fn metrics_follow_unicode_live_edits_and_history_without_materialization() {
        // Put CR/LF in separate original pieces and include non-ASCII whitespace/scalars.
        let source = format!("{}\r\n中文\u{2003}emoji😀 e\u{301}\n", "x".repeat(4_095));
        let mut fixture = Fixture::new(&source);
        assert_eq!(
            fixture.document.character_count(),
            source.replace("\r\n", "\n").chars().count()
        );
        assert_eq!(fixture.document.word_count(), 4);
        let before = fixture.document.storage_profile();

        // Insert letters inside an L/N run so original/edit fragments still contribute one word.
        fixture
            .document
            .insert_utf16(2, "中文", MarkdownTextSelection::caret(4))
            .expect("insert live word fragment");
        assert_eq!(
            fixture.document.character_count(),
            source.replace("\r\n", "\n").chars().count() + 2
        );
        assert_eq!(fixture.document.word_count(), 4);
        fixture.document.undo().expect("undo insertion");
        assert_eq!(
            fixture.document.character_count(),
            source.replace("\r\n", "\n").chars().count()
        );
        fixture.document.redo().expect("redo insertion");
        assert_eq!(fixture.document.word_count(), 4);

        // Reading aggregates changes neither appended edit storage nor bounded history.
        let after_edit = fixture.document.storage_profile();
        let _ = (
            fixture.document.character_count(),
            fixture.document.word_count(),
        );
        assert_eq!(fixture.document.storage_profile(), after_edit);
        assert!(after_edit.appended_edit_bytes > before.appended_edit_bytes);
    }

    /// Exact category edges and every scalar-aligned split match the original product's countWords method.
    #[test]
    fn word_counts_match_frozen_original_regexp_cases_and_all_piece_splits() {
        let reference = word_count_reference();
        let cases = reference["cases"].as_array().expect("reference cases");
        for case in cases {
            // Each expected value came from the actual preserved method running over CodeMirror state.doc.
            let text = case["text"].as_str().expect("reference text");
            let expected = case["expected"].as_u64().expect("reference count") as usize;
            let fixture = Fixture::new(text);
            let before = fixture.document.storage_profile();
            assert_eq!(fixture.document.word_count(), expected, "text={text:?}");
            assert_eq!(fixture.document.storage_profile(), before);

            // A cached root must preserve the same count regardless of the particular scalar piece split.
            for split in text
                .char_indices()
                .map(|(offset, _)| {
                    // Rust scalar boundaries are the only legal piece boundaries; no surrogate split is generated.
                    offset
                })
                .chain(std::iter::once(text.len()))
            {
                let joined = MarkdownDocumentMetrics::joined(
                    MarkdownDocumentMetrics::from_text(&text[..split]),
                    MarkdownDocumentMetrics::from_text(&text[split..]),
                );
                assert_eq!(joined.word_count, expected, "text={text:?}, split={split}");
            }
        }
    }

    /// Exhaustive scalar membership matches Node 24.21's Unicode 17 gc=L|N classifier, including new scripts.
    #[test]
    fn unicode_word_classifier_matches_all_original_regexp_scalar_values() {
        let reference = word_count_reference();
        let classifier = &reference["classifier"];
        assert_eq!(classifier["code_points"], 0x110000);
        let mut membership = vec![0u8; 0x110000 / 8];
        let mut scalars = 0u64;
        let mut members = 0u64;
        for point in 0..0x110000u32 {
            // Every valid scalar is checked; surrogate positions retain the same zero bits as the JS oracle.
            let Some(character) = char::from_u32(point) else {
                // Surrogates are not valid Rust text and do not participate in category membership.
                continue;
            };
            scalars += 1;
            if is_word_character(character) {
                // Encode exactly one Letter/Number membership bit in least-significant-bit-first order.
                membership[point as usize >> 3] |= 1 << (point & 7);
                members += 1;
            }
        }

        // A version/category-tagged digest covers all 1,112,064 scalar decisions without a duplicate table.
        let mut hasher = Sha256::new();
        hasher.update(b"omni-word-scalar-v1\0Unicode=17.0\0gc=L|N\0");
        hasher.update(membership);
        assert_eq!(classifier["scalar_values"], scalars);
        assert_eq!(classifier["member_count"], members);
        assert_eq!(classifier["sha256"], format!("{:x}", hasher.finalize()));
    }

    /// Pair/triple aggregation matches 11,132 original-method references and both associative groupings.
    #[test]
    fn word_run_aggregates_match_original_regexp_across_all_reference_joins() {
        let reference = word_count_reference();
        let boundaries = &reference["boundaries"];
        let metrics = boundaries["fragments"]
            .as_array()
            .expect("boundary fragments")
            .iter()
            .map(|fragment| {
                // The corpus mixes empty pieces, letters, number subclasses, marks, controls, emoji, and CRLF.
                MarkdownDocumentMetrics::from_text(fragment.as_str().expect("reference fragment"))
            })
            .collect::<Vec<_>>();
        let mut hasher = Sha256::new();
        hasher.update(b"omni-word-joins-v1\0Unicode=17.0\0gc=L|N\0");
        let mut cases = 0u64;
        for left in &metrics {
            // Pair results are encoded first in the generator's stable nested-loop order.
            for right in &metrics {
                // The tiny fragment corpus cannot overflow the oracle's fixed-width u32 count encoding.
                let count =
                    u32::try_from(MarkdownDocumentMetrics::joined(*left, *right).word_count)
                        .unwrap();
                hasher.update(count.to_le_bytes());
                cases += 1;
            }
        }
        for left in &metrics {
            // Triples check that AVL rotations/grouping cannot change the boundary-reconciled count.
            for middle in &metrics {
                // A mark-only middle piece separates words instead of becoming an Alphabetic continuation.
                for right in &metrics {
                    let left_grouped = MarkdownDocumentMetrics::joined(
                        MarkdownDocumentMetrics::joined(*left, *middle),
                        *right,
                    );
                    let right_grouped = MarkdownDocumentMetrics::joined(
                        *left,
                        MarkdownDocumentMetrics::joined(*middle, *right),
                    );
                    assert_eq!(left_grouped, right_grouped);
                    hasher.update(
                        u32::try_from(left_grouped.word_count)
                            .unwrap()
                            .to_le_bytes(),
                    );
                    cases += 1;
                }
            }
        }
        assert_eq!(boundaries["case_count"], cases);
        assert_eq!(boundaries["sha256"], format!("{:x}", hasher.finalize()));
    }

    /// Real persistent-tree edits merge/split category runs while undo/redo restore cached metrics.
    #[test]
    fn word_count_updates_across_original_edit_boundaries_and_history_without_mirrors() {
        // Put the hyphen at the end of an original 4 KiB leaf to exercise a real source-piece boundary.
        let mut fixture = Fixture::new(&format!("{}-right", "x".repeat(4_095)));
        assert_eq!(fixture.document.word_count(), 2);
        fixture
            .document
            .delete_utf16_range(4_095..4_096, MarkdownTextSelection::caret(4_095))
            .unwrap();
        assert_eq!(fixture.document.word_count(), 1);
        fixture.document.undo().expect("undo separator deletion");
        assert_eq!(fixture.document.word_count(), 2);
        fixture.document.redo().expect("redo separator deletion");
        assert_eq!(fixture.document.word_count(), 1);

        // Insert an Alphabetic combining mark plus emoji at the join; neither belongs to gc=L|N.
        fixture
            .document
            .insert_utf16(4_095, "\u{345}😀", MarkdownTextSelection::caret(4_098))
            .unwrap();
        assert_eq!(fixture.document.word_count(), 2);
        fixture.document.undo().expect("undo mark/emoji separator");
        assert_eq!(fixture.document.word_count(), 1);
        fixture.document.redo().expect("redo mark/emoji separator");
        assert_eq!(fixture.document.word_count(), 2);

        // Replacing the separator with a Unicode-17 letter joins original/edit/original leaves into one run.
        fixture
            .document
            .replace_utf16_range(
                4_095..4_098,
                "\u{11db0}",
                MarkdownTextSelection::caret(4_097),
            )
            .unwrap();
        assert_eq!(fixture.document.word_count(), 1);
        fixture.document.undo().expect("undo Unicode-17 letter");
        assert_eq!(fixture.document.word_count(), 2);
        fixture.document.redo().expect("redo Unicode-17 letter");
        assert_eq!(fixture.document.word_count(), 1);

        // Repeated cached queries add neither edit storage nor history entries or a document mirror.
        let before = fixture.document.storage_profile();
        for _ in 0..100 {
            // Query work reads the root aggregate and never reclassifies the complete live text.
            assert_eq!(fixture.document.word_count(), 1);
        }
        assert_eq!(fixture.document.storage_profile(), before);
    }

    /// Search carries KMP prefix state over original and newly inserted pieces.
    #[test]
    fn search_crosses_piece_joins_and_reports_original_utf16_ranges() {
        // The first N ends an original piece; inserted ee and following dle live in other pieces.
        let mut fixture = Fixture::new(&format!("{}Ndle😀 中文 tail", "x".repeat(4_095)));
        fixture
            .document
            .insert_utf16(4_096, "ee", MarkdownTextSelection::caret(4_098))
            .expect("insert query middle");
        let document = &fixture.document;
        assert_eq!(
            document.find_utf16("needle", 0, false, false).unwrap(),
            Some(4_095..4_101)
        );
        assert_eq!(document.find_utf16("needle", 0, false, true).unwrap(), None);
        assert_eq!(
            document.find_utf16("Needle", 0, false, true).unwrap(),
            Some(4_095..4_101)
        );
        assert_eq!(
            document.find_utf16("😀 中文", 0, false, true).unwrap(),
            Some(4_101..4_106)
        );

        // A surrogate-interior or out-of-range caret is rejected without repairing its location.
        assert_eq!(
            document.find_utf16("tail", 4_102, false, true).err(),
            Some(MarkdownDocumentBufferError::NotUnicodeBoundary)
        );
        assert_eq!(
            document
                .find_utf16("tail", document.utf16_len() + 1, false, true)
                .err(),
            Some(MarkdownDocumentBufferError::RangeOutsideDocument)
        );
    }

    /// Direction bounds and ASCII-only folding remain explicit at overlapping matches.
    #[test]
    fn direction_case_empty_and_overlap_semantics_are_deterministic() {
        // Overlaps distinguish the last prefix match from the first forward match.
        let fixture = Fixture::new("ababa Ä ä ABC abc");
        let document = &fixture.document;
        assert_eq!(
            document.find_utf16("aba", 0, false, true).unwrap(),
            Some(0..3)
        );
        assert_eq!(
            document.find_utf16("aba", 1, false, true).unwrap(),
            Some(2..5)
        );
        assert_eq!(
            document.find_utf16("aba", 5, true, true).unwrap(),
            Some(2..5)
        );
        assert_eq!(
            document.find_utf16("aba", 4, true, true).unwrap(),
            Some(0..3)
        );

        // ASCII case folding preserves widths; non-ASCII text retains literal matching.
        assert_eq!(
            document.find_utf16("abc", 0, false, false).unwrap(),
            Some(10..13)
        );
        assert_eq!(
            document.find_utf16("abc", 0, false, true).unwrap(),
            Some(14..17)
        );
        assert_eq!(
            document.find_utf16("ä", 0, false, false).unwrap(),
            Some(8..9)
        );
        assert_eq!(document.find_utf16("", 0, false, false).unwrap(), None);
        assert_eq!(document.find_utf16("aba", 0, true, true).unwrap(), None);
        assert_eq!(
            document
                .find_utf16("aba", document.utf16_len(), false, true)
                .unwrap(),
            None
        );
    }
}
