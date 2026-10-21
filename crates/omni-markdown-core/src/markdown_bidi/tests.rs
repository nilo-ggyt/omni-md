//! Complete strings belong only to finite synthetic oracles; large fixtures are written in fixed chunks without constructing a product whole-text array.

use super::*;
use crate::MarkdownDocumentBuffer;
use unicode_bidi::utf16::BidiInfo;

#[cfg(feature = "paged-layout-breaks")]
/// Read an actual page before using the existing test reclaim failure seam; returning a pin alone does not prove msync occurred.
fn poison_original_read_windows(document: &MarkdownDocumentBuffer) {
    document
        .text_snapshot()
        .visit_byte_range(0..1, |_| Ok(()))
        .unwrap();
    document.fail_next_original_reclaim_for_test();
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    ); // Only after actual unpinned-page reclamation fails should later algorithms observe this same permanent Source error.
}

fn fixture(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    // Only this private synthetic file enters the actual document/mmap implementation; the TempDir owns its fixture path.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bidi.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

#[test]
fn production_resource_observer_distinguishes_uniform_from_paged_and_keeps_neither_alive() {
    // Compare actual resource paths: Uniform owns no metadata file, while the original analysis still holds Source until its last owner drops.
    for (text, paged) in [("ASCII A123", false), ("אב😀 A123", true)] {
        let (_directory, document) = fixture(text);
        let mapping = document.original_backing_observer().unwrap();
        let analyzed = MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..document.utf16_len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
        )
        .unwrap();
        let observer = analyzed.resource_observer();
        let before = observer.snapshot();
        assert_eq!(before.file_created, paged);
        assert_eq!(before.file_open, paged);
        assert!(!before.close_succeeded); // Uniform has no FD to close; a live Paged owner cannot report premature release either.
        assert_eq!(before.cache_bytes, if paged { 65536 } else { 0 });
        drop(document);
        assert!(mapping.mapped_bytes() > 0); // Analysis still owns the original frozen Source; an atomic observer does not release that ownership.
        drop(analyzed);
        let after = observer.snapshot();
        assert!(!after.file_open && after.close_succeeded == paged);
        assert_eq!((after.close_errno, after.cache_bytes), (None, 0));
        assert!(observer.is_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn observed_analysis_returns_the_actual_constructor_cleanup_receipt_after_cancellation() {
    // Keep the existing first-page barrier on the real worker, then cancel/resume and join before checking actual constructor cleanup.
    let (_directory, document) = fixture("אב😀 A123");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let end = source.utf16_len();
    let source_observer = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let (cancellation, gate) = MarkdownBidiCancellation::pause_after_first_page();
    let worker_cancel = cancellation.clone();
    let worker = std::thread::spawn(move || {
        MarkdownBidiParagraph::analyze_layout_source_cancellable_observed(
            source,
            &authority,
            0..end,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &worker_cancel,
        )
    });
    gate.entered.wait(); // Continue only after the actual FD and first 16-KiB page exist, without a delay-based progress guess.
    let original = gate.file.lock().unwrap().as_ref().unwrap().clone();
    let live = original.snapshot();
    assert!(live.file_created && live.file_open && !live.close_succeeded);
    assert_eq!((live.cache_bytes, live.peak_cache_bytes), (16384, 16384));
    cancellation.cancel();
    gate.resume.wait();
    let (result, observer) = worker.join().unwrap();
    assert!(matches!(result, Err(BidiError::Cancelled)));
    let actual = observer.snapshot();
    assert!(actual.file_created && !actual.file_open && actual.close_succeeded);
    assert_eq!((actual.cache_bytes, actual.peak_cache_bytes), (0, 16384));
    assert!(original.closed() && observer.is_released());
    drop(document);
    assert!(source_observer.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn observed_uniform_and_failed_source_never_claim_a_file_was_created_or_closed() {
    // A complete Uniform result and rejection before allocation both have no metadata FD, but only the former owns a healthy result Source.
    let (_directory, document) = fixture("ASCII A123");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let (result, uniform) = MarkdownBidiParagraph::analyze_layout_source_cancellable_observed(
        source.clone(),
        &authority,
        0..source.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
        &MarkdownBidiCancellation::new(),
    );
    let value = result.unwrap();
    assert!(matches!(value.resolution, Resolution::UniformZero(_)));
    assert!(!uniform.snapshot().file_created && !uniform.snapshot().close_succeeded);
    assert!(uniform.is_released()); // Only metadata files/pages are absent; the analysis still owns Source, which is a distinct lifetime.
    drop(value);
    let foreign = crate::MarkdownLayoutTextSource::capture(&document).authority();
    poison_original_read_windows(&document);
    let cancellation = MarkdownBidiCancellation::new();
    cancellation.cancel();
    let (result, failed) = MarkdownBidiParagraph::analyze_layout_source_cancellable_observed(
        source,
        &foreign,
        0..document.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
        &cancellation,
    );
    assert!(matches!(
        result,
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    )); // Actual shared EIO precedes a foreign ticket or cancellation; resource observations cannot rewrite the original failure cause.
    let actual = failed.snapshot();
    assert!(!actual.file_created && !actual.file_open && !actual.close_succeeded);
    assert_eq!(
        (
            actual.close_errno,
            actual.cache_bytes,
            actual.peak_cache_bytes
        ),
        (None, 0, 0)
    );
    assert!(failed.is_released());
}

#[test]
fn paragraph_lookup_uses_original_table_and_matches_finite_uba_oracle_at_scalar_boundaries() {
    // Check actual UBA paragraph ranges against a finite independent oracle, preserving CR/LF and supplementary-scalar coordinate semantics.
    for text in [
        "",
        "纯LTR中文😀123",
        "a\r\nb\u{85}c\u{2029}אב😀\n",
        "\n\n末段",
    ] {
        let (_directory, document) = fixture(text);
        let units: Vec<u16> = text.encode_utf16().collect();
        let oracle = BidiInfo::new_with_data_source(&UnicodeBidiData, &units, None);
        let mut actual = MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..units.len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
        )
        .unwrap();
        let mut offset = 0;
        for scalar in text.chars() {
            let expected = oracle
                .paragraphs
                .iter()
                .find(|paragraph| paragraph.range.contains(&offset))
                .unwrap()
                .range
                .clone();
            assert_eq!(
                actual.paragraph_range_at_utf16(offset).unwrap(),
                Some(expected)
            );
            if scalar.len_utf16() == 2 {
                assert!(matches!(
                    actual.paragraph_range_at_utf16(offset + 1),
                    Err(BidiError::InvalidScalarBoundary)
                ));
            } // A low surrogate may have a level record, but cannot become a new item's paragraph or geometry boundary.
            offset += scalar.len_utf16();
        }
        assert_eq!(actual.paragraph_range_at_utf16(units.len()).unwrap(), None);
        assert!(matches!(
            actual.paragraph_range_at_utf16(units.len() + 1),
            Err(BidiError::InvalidRange)
        ));
    }
    // Nonzero analysis ranges still return absolute original UTF16 coordinates; range-relative zero cannot become a newly certified document paragraph start.
    let (_directory, document) = fixture("前😀甲\nאב\n尾");
    let start = "前😀".encode_utf16().count();
    let end = document.utf16_len() - 1;
    let mut actual = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        start..end,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    assert_eq!(
        actual.paragraph_range_at_utf16(start).unwrap(),
        Some(start..start + 2)
    );
    assert_eq!(
        actual.paragraph_range_at_utf16(start + 2).unwrap(),
        Some(start + 2..end)
    );
    assert!(matches!(
        actual.paragraph_range_at_utf16(start - 1),
        Err(BidiError::InvalidRange)
    ));
    assert_eq!(actual.paragraph_range_at_utf16(end).unwrap(), None);
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn paragraph_lookup_preserves_cancellation_and_original_failure_priority_even_at_eof() {
    // EOF and cached paragraph lookups must recheck actual Source/cancellation for both representations, rather than bypassing their authority.
    for text in ["纯LTR中文😀123", "aא\nb"] {
        let (_directory, document) = fixture(text);
        let source = crate::MarkdownLayoutTextSource::capture(&document);
        let cancellation = MarkdownBidiCancellation::new();
        let mut actual = MarkdownBidiParagraph::analyze_layout_source_cancellable(
            source.clone(),
            &source.authority(),
            0..source.utf16_len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
            &cancellation,
        )
        .unwrap();
        assert!(actual.paragraph_range_at_utf16(0).unwrap().is_some());
        cancellation.cancel();
        assert!(matches!(
            actual.paragraph_range_at_utf16(0),
            Err(BidiError::Cancelled)
        ));
        assert!(matches!(
            actual.paragraph_range_at_utf16(source.utf16_len()),
            Err(BidiError::Cancelled)
        ));
        poison_original_read_windows(&document);
        assert!(matches!(
            actual.paragraph_range_at_utf16(source.utf16_len()),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
    } // Neither Uniform nor actual paging may hide known Source failure through EOF or a previous successful lookup.
}

fn compare(source: &str, base: BidiBaseDirection, all_lines: bool) {
    // Limit the independent full-array oracle explicitly, then compare actual paragraph levels and all requested scalar-aligned soft-line splits.
    assert!(
        source.chars().count() <= 4096,
        "完整oracle只能处理明确有限的测试数据"
    );
    let (_directory, document) = fixture(source);
    let original: Vec<u16> = source.encode_utf16().collect();
    let reference = BidiInfo::new_with_data_source(
        &UnicodeBidiData,
        &original,
        base.level()
            .map(|level| unicode_bidi::Level::new(level).unwrap()),
    );
    let mut actual = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        0..document.utf16_len(),
        base,
        BidiMemoryBudget::new(4 * store::PAGE_BYTES).unwrap(),
    )
    .unwrap();
    for (index, level) in reference.levels.iter().enumerate() {
        assert_eq!(
            actual.level_at_utf16(index).unwrap(),
            level.number(),
            "source={source:?}, unit={index}, base={base:?}"
        );
    }
    assert_eq!(actual.actual_paragraph_count(), reference.paragraphs.len());
    let mut scalar_offsets = Vec::new();
    let mut offset = 0;
    for scalar in source.chars() {
        scalar_offsets.push(offset);
        offset += scalar.len_utf16();
    }
    scalar_offsets.push(offset);
    for paragraph in &reference.paragraphs {
        // L1/L2 expectations belong to each actual line range; paragraph-level pre-L1 levels alone cannot prove visual order.
        let start = scalar_offsets.partition_point(|&offset| offset < paragraph.range.start);
        let end = scalar_offsets.partition_point(|&offset| offset < paragraph.range.end);
        let split_points: Vec<usize> = if all_lines {
            (start..=end).collect()
        } else {
            vec![start, end]
        };
        for split in split_points {
            for ordinals in [start..split, split..end] {
                if ordinals.is_empty() {
                    continue;
                }
                let range = scalar_offsets[ordinals.start]..scalar_offsets[ordinals.end];
                let levels = reference.reordered_levels_per_char(paragraph, range.clone());
                let expected: BidiVisualOracle =
                    BidiVisualOracle::new(&scalar_offsets, &levels, ordinals.clone());
                let mut visual = Vec::new();
                actual
                    .visit_visual_runs(range, |run| {
                        let from = scalar_offsets
                            .binary_search(&run.source_utf16_start)
                            .unwrap();
                        let to = scalar_offsets.binary_search(&run.source_utf16_end).unwrap();
                        if run.level & 1 == 0 {
                            visual.extend(from..to);
                        } else {
                            visual.extend((from..to).rev());
                        }
                        for &index in &scalar_offsets[from..to] {
                            assert_eq!(
                                run.level,
                                levels[scalar_offsets.binary_search(&index).unwrap()].number()
                            );
                        }
                    })
                    .unwrap();
                assert_eq!(
                    visual, expected.order,
                    "L1/L2 source={source:?}, base={base:?}, scalars={ordinals:?}"
                );
            }
        }
    }
    let profile = actual.storage_profile();
    match &actual.resolution {
        // Output already matched the independent oracle; now distinguish actual resource configurations without inventing a Uniform cache.
        Resolution::UniformZero(_) => {
            assert_eq!(profile.cache_bytes, 0);
            assert_eq!(profile.page_loads, 0);
        }
        Resolution::Paged(_) => assert_eq!(profile.cache_bytes, 4 * store::PAGE_BYTES),
        #[cfg(feature = "paged-shaping-frontier")]
        Resolution::VisibleParagraphs(_) => panic!("原完整分析 oracle 不构造显式折叠结果"),
    }
    assert!(profile.source_borrow_max_bytes <= 4096);
    assert_eq!(profile.unicode_version, (18, 0, 0)); // This is the actual property provider's version, not the algorithm crate's bundled data version.
}

struct BidiVisualOracle {
    order: Vec<usize>,
}
impl BidiVisualOracle {
    fn new(_offsets: &[usize], levels: &[unicode_bidi::Level], ordinals: Range<usize>) -> Self {
        // Build only the finite oracle's expected scalar order, independently of the production streamed run visitor.
        Self {
            order: BidiInfo::reorder_visual(&levels[ordinals.clone()])
                .into_iter()
                .map(|index| ordinals.start + index)
                .collect(),
        }
    }
}

#[test]
fn fixed_contexts_and_every_scalar_line_boundary_match_utf16_oracle() {
    // Exercise complete real contexts across scripts, isolates, controls, combining scalars, and brackets at every scalar-aligned line split.
    for source in [
        "",
        "abc 中文123",
        "אבג 123 Arabic العربية xyz",
        "אבגדה (12345 -- anchor Latin 67890) 再次中文 العربية",
        "a [ב (12 العربية) ג] z",
        "a\u{2329}אבג\u{3009} z \u{3008}العربية\u{232a}",
        "a\u{2068}123 العربية\u{2069}z",
        "א\u{2068}123 English\u{2069}ב",
        "a\u{2067}ב\u{2066}x12\u{2069}ג\u{2069}z",
        "a\u{2069}b\u{2068}123 العربية",
        "a\u{202b}ב12\u{202a}x\u{202c}ג\u{202c}z",
        "a\u{202e}abc123\u{202c}ב\u{202d}ג45\u{202c}",
        "a e\u{0301} א\u{05b0} العربية\u{064e} 👩🏽‍💻",
        "🙂 𝟙 𞤀 𝔄 \u{e0100} אבג",
        "a\0\u{2060}ב\u{200d}12\u{00ad}c",
        "a ב 123  \t  x\u{2028}ג  \n",
        "a ב\r\n العربية\u{2029}123\u{0085}x",
        "אב\u{2066}12\u{2069}($+\u{2060}--123)\u{2066}z\u{2069}גד",
        // New bracket data still uses original physical-paragraph N0 context; strong text inside an isolate must not leak outside it.
        "a ב\u{2e62}ג\u{2e63}",
        "a \u{2067}ב\u{2e62}12\u{2e63}ג\u{2069} z",
        // Default AL/BN differences must pass the complete rule oracle too, not only a property-table lookup check.
        "a\u{086b}123\u{2065}ב\u{fdd0}z\u{10ffff}",
        "a\u{05c8}ב\u{1b3a}123\u{1acf}z",
    ] {
        for base in [
            BidiBaseDirection::Auto,
            BidiBaseDirection::LeftToRight,
            BidiBaseDirection::RightToLeft,
        ] {
            compare(source, base, true);
        }
    }
}

#[test]
fn deterministic_property_sequences_match_complete_context_without_text_mirror() {
    // Reproducible finite combinations use the same independent oracle without installing a second text model into the production analysis.
    let symbols = [
        'a', 'א', 'ع', '1', '\u{0661}', '+', '$', ',', ' ', '\u{0300}', '\u{2060}', '(', ')', '[',
        ']', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{202a}', '\u{202b}', '\u{202c}',
        '\u{202d}', '\u{202e}', '🙂',
    ];
    let mut seed = 0x91df_732a_u64;
    for _ in 0..128 {
        let mut source = String::new();
        for _ in 0..64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            source.push(symbols[(seed >> 32) as usize % symbols.len()]);
        }
        for base in [
            BidiBaseDirection::Auto,
            BidiBaseDirection::LeftToRight,
            BidiBaseDirection::RightToLeft,
        ] {
            compare(&source, base, false);
        }
    }
}

#[test]
fn isolate_and_embedding_overflow_use_paged_initial_stack_and_exact_standard_levels() {
    // Beyond the legal explicit-level depth, preserve actual overflow/isolate behavior rather than truncating source or the initial paged stack.
    for source in [
        format!("a{}ב{}z", "\u{2067}".repeat(140), "\u{2069}".repeat(141)),
        format!("a{}12ב{}z", "\u{202b}".repeat(140), "\u{202c}".repeat(141)),
    ] {
        for base in [BidiBaseDirection::Auto, BidiBaseDirection::RightToLeft] {
            compare(&source, base, false);
        }
    }
}

#[test]
fn immutable_analysis_survives_edit_and_document_drop_and_preserves_surrogate_contracts() {
    // Editing/dropping the active document cannot change the original analysis; low-surrogate level reads remain distinct from valid range endpoints.
    let (_directory, mut document) = fixture("x🙂אב12中z");
    let mut value = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        1..9,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    let low = value.level_at_utf16(2).unwrap();
    assert_eq!(low, value.level_at_utf16(1).unwrap());
    assert!(matches!(
        value.visit_logical_runs(2..3, |_| {}),
        Err(BidiError::InvalidScalarBoundary)
    ));
    document
        .replace_utf16_range(
            0..document.utf16_len(),
            "later",
            crate::MarkdownTextSelection::caret(5),
        )
        .unwrap();
    drop(document);
    assert_eq!(value.level_at_utf16(2).unwrap(), low);
    let mut runs = 0;
    value.visit_visual_runs(1..9, |_| runs += 1).unwrap();
    assert!(runs > 0);
}

#[test]
fn empty_eof_invalid_ranges_and_budget_never_publish_incomplete_analysis() {
    // Actual empty analysis emits no run, while unsupported budgets and scalar/range violations cannot publish a partially valid result.
    let (_directory, document) = fixture("");
    let mut empty = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        0..0,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    assert!(empty.level_at_utf16(0).is_err());
    empty
        .visit_visual_runs(0..0, |_| panic!("空范围没有run"))
        .unwrap();
    assert!(BidiMemoryBudget::new(0).is_err());
    assert!(BidiMemoryBudget::new(2 * 1024 * 1024).is_err());
    let (_directory, document) = fixture("🙂");
    assert!(matches!(
        MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            1..2,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default()
        ),
        Err(BidiError::InvalidScalarBoundary)
    ));
    assert!(matches!(
        MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..3,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default()
        ),
        Err(BidiError::InvalidRange)
    ));
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
/// Another capture of equal text/generation has no original publication authority; the actual capsule preserves frozen values and mapping lifetime across edits.
fn shared_layout_source_authority_and_snapshot_alias_keep_the_original_bidi_value() {
    let (_directory, mut document) = fixture("x🙂אב12中z");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let other = crate::MarkdownLayoutTextSource::capture(&document);
    let foreign = other.authority();
    let payload = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    assert!(matches!(
        MarkdownBidiParagraph::analyze_layout_source(
            source.clone(),
            &foreign,
            0..source.utf16_len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
        ),
        Err(BidiError::SourceMismatch)
    ));
    let mut value = MarkdownBidiParagraph::analyze_layout_source(
        source.clone(),
        &authority,
        0..source.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    value.require_layout_source(&authority).unwrap();
    assert!(matches!(
        value.require_layout_source(&foreign),
        Err(BidiError::SourceMismatch)
    ));
    let original_level = value.level_at_utf16(3).unwrap();
    document
        .replace_utf16_range(
            0..document.utf16_len(),
            "later",
            crate::MarkdownTextSelection::caret(5),
        )
        .unwrap();
    drop(document);
    drop(source);
    drop(other);
    assert!(!payload.payload_released());
    assert!(!mapping.unmap_succeeded());
    value.require_layout_source(&authority).unwrap();
    assert_eq!(value.level_at_utf16(3).unwrap(), original_level);
    drop(value);
    assert!(payload.payload_released());
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(!mapping.failed());
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
/// Initial endpoint conversion and completed analysis retain actual reclaim errno, instead of returning a misleading boundary error or cached level.
fn bidi_source_reclaim_failure_is_not_hidden_by_scalar_validation_or_cached_levels() {
    let (_directory, document) = fixture("abאב中🙂");
    poison_original_read_windows(&document);
    let failed = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        1..document.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    );
    assert!(matches!(
        failed,
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    ));

    let (_other_directory, other_document) = fixture("abאב中🙂");
    let source = crate::MarkdownLayoutTextSource::capture(&other_document);
    let authority = source.authority();
    let mut value = MarkdownBidiParagraph::analyze_layout_source(
        source.clone(),
        &authority,
        0..source.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    poison_original_read_windows(&other_document);
    assert!(source.visit_byte_range(0..1, |_| Ok(())).is_err());
    assert!(matches!(
        value.level_at_utf16(0),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    ));
    assert!(matches!(
        value.visit_visual_runs(0..0, |_| {}),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    ));
    assert!(matches!(
        value.require_layout_source(&authority),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    ));
}

#[test]
fn giant_generated_paragraph_keeps_source_borrow_and_total_page_cache_bounded() {
    // Write a large private fixture in fixed chunks and analyze through production; the bounded component page cache is not a whole-product RSS claim.
    use std::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("giant.md");
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..4096 {
        file.write_all("中文abc אבג العربية 12345 (67890) 🙂 ".as_bytes())
            .unwrap();
    }
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    let mut value = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        0..document.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(4 * store::PAGE_BYTES).unwrap(),
    )
    .unwrap();
    let profile = value.storage_profile();
    assert!(profile.utf16_units > 100_000);
    assert!(profile.source_borrow_max_bytes <= 4096);
    assert_eq!(profile.cache_bytes, 65536);
    let mut visited = 0;
    value.visit_logical_runs(0..100, |_| visited += 1).unwrap();
    assert!(visited > 0);
}

#[test]
fn anonymous_metadata_descriptor_is_released_after_last_value() {
    const CHILD: &str = "OMNI_BIDI_RELEASE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Wait for an isolated test child to avoid concurrent FD-number reuse being misclassified as a leak.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "markdown_bidi::tests::anonymous_metadata_descriptor_is_released_after_last_value",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let (_directory, document) = fixture("abc אבג العربية 123");
    let mut value = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        0..document.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    assert!(matches!(value.resolution, Resolution::Paged(_))); // Actual R/AL context must acquire the original anonymous FD; a zero-resource Uniform result cannot bypass this release scenario.
    let fd = value.resolution.paged_mut().raw_fd();
    drop(document);
    // SAFETY: F_GETFD observes the test's actual exclusive descriptor without reading contents or transferring/changing ownership.
    assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    drop(value);
    // SAFETY: This process has created no replacement FD; F_GETFD safely reports EBADF for a closed number without accessing a freed Rust object.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
}

#[test]
fn visitor_reports_partial_candidates_but_failed_batch_and_every_later_query_are_revoked() {
    // Retain the existing partial-read failure seam: callbacks may have produced candidates, but only overall Ok can authorize the complete batch.
    let source = "aא".repeat(2048);
    let (_directory, document) = fixture(&source);
    let mut value = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        0..document.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(4 * store::PAGE_BYTES).unwrap(),
    )
    .unwrap();
    value.level_at_utf16(0).unwrap(); // Warm the actual original page so initial runs can be observed before the next page's I/O failure.
    value.resolution.paged_mut().fail_next_read_after_prefix(8);
    let mut candidates = 0;
    let result = value.visit_logical_runs(0..document.utf16_len(), |_| candidates += 1);
    assert!(candidates > 0);
    assert!(matches!(&result, Err(BidiError::Storage(_))));
    let committed = if result.is_ok() { candidates } else { 0 };
    assert_eq!(committed, 0); // A candidate callback is not complete Ready; the caller must wait for the entire method's Ok before publication.
    assert!(matches!(
        value.level_at_utf16(0),
        Err(BidiError::PoisonedMetadata)
    ));
    assert!(matches!(
        value.visit_visual_runs(0..0, |_| {}),
        Err(BidiError::PoisonedMetadata)
    ));
}

#[test]
#[ignore = "需显式提供OMNI_BIDI_CHARACTER_TEST官方语料；语料与当前属性版本分别记录"]
fn official_bidi_character_test_levels_and_order_are_exact() {
    // This explicitly ignored oracle requires a supplied official corpus and records its own version separately from current property data.
    use std::io::BufRead;
    let path = std::env::var_os("OMNI_BIDI_CHARACTER_TEST").expect("必须显式指定官方数据路径");
    let file = std::fs::File::open(path).unwrap();
    let mut count = 0;
    let mut corpus_version = None;
    for (line_number, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.unwrap();
        if let Some(version) = line
            .strip_prefix("# BidiCharacterTest-")
            .and_then(|value| value.strip_suffix(".txt"))
        {
            corpus_version = Some(version.to_owned());
        }
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split(';').map(str::trim).collect();
        assert_eq!(fields.len(), 5, "官方数据行{}", line_number + 1);
        let source: String = fields[0]
            .split_whitespace()
            .map(|word| char::from_u32(u32::from_str_radix(word, 16).unwrap()).unwrap())
            .collect();
        assert!(source.chars().count() <= 4096, "官方oracle仍必须显式有限");
        let base = match fields[1] {
            "0" => BidiBaseDirection::LeftToRight,
            "1" => BidiBaseDirection::RightToLeft,
            "2" => BidiBaseDirection::Auto,
            _ => panic!("官方方向字段无效"),
        };
        let (_directory, document) = fixture(&source);
        let mut value = MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..document.utf16_len(),
            base,
            BidiMemoryBudget::new(4 * store::PAGE_BYTES).unwrap(),
        )
        .unwrap();
        assert_eq!(
            value.actual_paragraph_count(),
            1,
            "官方单段输入行{}",
            line_number + 1
        );
        let paragraph_base = match &mut value.resolution {
            Resolution::UniformZero(_) => 0,
            Resolution::Paged(store) => Paragraph::read(store, 0).unwrap().base,
            #[cfg(feature = "paged-shaping-frontier")]
            Resolution::VisibleParagraphs(_) => panic!("官方单段 oracle 不构造显式折叠结果"),
        };
        assert_eq!(
            paragraph_base,
            fields[2].parse::<u8>().unwrap(),
            "P2/P3行{}",
            line_number + 1
        );
        let expected_levels: Vec<_> = fields[3].split_whitespace().collect();
        // Only one explicitly bounded official case builds scalar/UTF16 expectations; product analysis continues through the actual Source API.
        let mut offsets = Vec::new();
        let mut offset = 0;
        for scalar in source.chars() {
            offsets.push(offset);
            offset += scalar.len_utf16();
        }
        offsets.push(offset);
        assert_eq!(offsets.len() - 1, expected_levels.len());
        // Official cases treat the whole input as one line and include L1 levels; do not compare them with analyze's paragraph-level pre-L1 result.
        let mut actual_levels = vec![None; expected_levels.len()];
        let mut actual_order = Vec::new();
        value
            .visit_visual_runs(0..offset, |run| {
                let first = offsets.binary_search(&run.source_utf16_start).unwrap();
                let last = offsets.binary_search(&run.source_utf16_end).unwrap();
                actual_levels[first..last].fill(Some(run.level));
                let mut emit = |scalar| {
                    if expected_levels[scalar] != "x" {
                        actual_order.push(scalar);
                    }
                };
                if run.level & 1 == 0 {
                    for scalar in first..last {
                        emit(scalar);
                    }
                } else {
                    for scalar in (first..last).rev() {
                        emit(scalar);
                    }
                }
            })
            .unwrap();
        for (scalar, expected) in expected_levels.iter().enumerate() {
            if *expected != "x" {
                assert_eq!(
                    actual_levels[scalar],
                    Some(expected.parse::<u8>().unwrap()),
                    "L1后level行{} scalar{}",
                    line_number + 1,
                    scalar
                );
            }
        }
        let expected_order: Vec<usize> = fields[4]
            .split_whitespace()
            .map(|word| word.parse().unwrap())
            .collect();
        assert_eq!(
            actual_order,
            expected_order,
            "L1/L2官方行{}",
            line_number + 1
        );
        count += 1;
    }
    assert!(count > 0);
    let corpus_version =
        corpus_version.expect("官方语料必须保留版本头，不能根据provider猜测语料版本");
    eprintln!(
        "bidi_official_character_test corpus={corpus_version} properties={:?} cases={count} source_payload_mirror=false",
        unicode_data::UNICODE_VERSION,
    );
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn cancellation_before_allocation_and_empty_source_never_returns_ready() {
    // A pre-cancelled request fails before metadata allocation even for an empty Source, then releases its original snapshot/backing claim.
    for text in ["", "中文😀 אב12"] {
        let (_directory, document) = fixture(text);
        let source = crate::MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let observer = source.resource_observer();
        let mapping = document.original_backing_observer();
        let end = source.utf16_len();
        drop(document);
        let cancellation = MarkdownBidiCancellation::new();
        cancellation.cancel();
        assert!(matches!(
            MarkdownBidiParagraph::analyze_layout_source_cancellable(
                source,
                &authority,
                0..end,
                BidiBaseDirection::Auto,
                BidiMemoryBudget::default(),
                &cancellation,
            ),
            Err(BidiError::Cancelled)
        ));
        assert!(observer.payload_released());
        if let Some(mapping) = mapping {
            assert!(mapping.unmap_succeeded());
            assert_eq!(mapping.mapped_bytes(), 0);
            assert!(!mapping.failed());
        }
        assert!(cancellation.is_cancelled());
    }
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn cancel_at_actual_first_allocation_joins_and_releases_same_source_and_real_fd() {
    // The existing barrier synchronizes the actual first allocation, not elapsed time; join the same worker before checking Source and FD release.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<crate::MarkdownLayoutTextSource>();
    assert_send_sync::<crate::MarkdownLayoutTextAuthority>();
    assert_send_sync::<MarkdownBidiCancellation>();
    let (_directory, document) = fixture("非零前缀\n中文🙂 \u{2067}אב12\u{2069} نهاية");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let observer = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let end = source.utf16_len();
    drop(document);
    let (cancellation, pause) = MarkdownBidiCancellation::pause_after_first_page();
    let worker_cancellation = cancellation.clone();
    let worker = std::thread::spawn(move || {
        MarkdownBidiParagraph::analyze_layout_source_cancellable(
            source,
            &authority,
            6..end,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
            &worker_cancellation,
        )
    });
    pause.entered.wait(); // The actual first fixed page and anonymous FD exist; analysis is not yet permitted to continue.
    let fd = pause.file.lock().unwrap().as_ref().unwrap().clone();
    assert!(!fd.closed());
    assert!(!observer.payload_released());
    assert!(!mapping.unmap_succeeded());
    cancellation.cancel();
    pause.resume.wait();
    assert!(matches!(worker.join().unwrap(), Err(BidiError::Cancelled)));
    assert!(fd.closed()); // Actual libc close returned zero; this is not inferred from an FD number, delay, or thread completion alone.
    assert!(observer.payload_released());
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(!mapping.failed());
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn ready_cancellation_revokes_cached_empty_and_final_callback_queries() {
    // Cached, empty-range, and final-callback paths all retain cancellation checks; a callback's cancellation prevents overall successful publication.
    let (_directory, document) = fixture("prefix\nאב🙂 123中文");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let cancellation = MarkdownBidiCancellation::new();
    let end = source.utf16_len();
    let mut value = MarkdownBidiParagraph::analyze_layout_source_cancellable(
        source,
        &authority,
        7..end,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
        &cancellation,
    )
    .unwrap();
    value.require_layout_source(&authority).unwrap();
    value.level_at_utf16(7).unwrap(); // Warm the actual page; a cached hit after cancellation cannot provide an alternative success path.
    let fd = value.resolution.paged_mut().io_observer_for_test();
    cancellation.cancel();
    assert!(matches!(value.level_at_utf16(7), Err(BidiError::Cancelled)));
    assert!(matches!(
        value.require_layout_source(&authority),
        Err(BidiError::Cancelled)
    ));
    assert!(matches!(
        value.visit_logical_runs(7..7, |_| panic!("不应交付候选")),
        Err(BidiError::Cancelled)
    ));
    assert!(matches!(
        value.visit_visual_runs(7..7, |_| panic!("不应交付候选")),
        Err(BidiError::Cancelled)
    ));
    assert!(!fd.closed()); // Cancellation revokes authority without mutating immutable values or prematurely releasing the still-owned Store.
    drop(value);
    assert!(fd.closed());

    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let cancellation = MarkdownBidiCancellation::new();
    let mut value = MarkdownBidiParagraph::analyze_layout_source_cancellable(
        source,
        &authority,
        7..end,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
        &cancellation,
    )
    .unwrap();
    let result = value.visit_logical_runs(7..8, |_| cancellation.cancel());
    assert!(matches!(result, Err(BidiError::Cancelled))); // Revocation inside the final callback still prevents a complete-coverage Ok.
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn cancellation_preserves_already_failed_original_source_cause() {
    // Establish actual reclaim EIO first; subsequent cancellation cannot replace that original typed Source failure.
    let (_directory, document) = fixture("真实源错误🙂");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let end = source.utf16_len();
    poison_original_read_windows(&document);
    assert!(document.utf16_code_unit_at(0).is_err());
    let cancellation = MarkdownBidiCancellation::new();
    cancellation.cancel();
    assert!(matches!(
        MarkdownBidiParagraph::analyze_layout_source_cancellable(
            source,
            &authority,
            0..end,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
            &cancellation
        ),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
}

#[test]
fn uniform_zero_certificate_matches_oracle_and_reports_no_metadata_resources() {
    // Compare finite eligible contexts with actual UBA output first; no metadata allocation is a separate observation, not the proof of correctness.
    for text in [
        "",
        "中文abc123🙂",
        "\u{2060}\u{200d}\0\u{00ad}",
        "\u{0301}\u{0300}123",
        "$++,,123.45---$\u{2060}67\t  ",
        "a\t\u{001f}b\u{2028} ",
        "中文123\n",
        "中文123\r",
        "中文123\u{0085}",
        "中文123\u{2029}",
        "中文123\u{001c}",
    ] {
        for base in [BidiBaseDirection::Auto, BidiBaseDirection::LeftToRight] {
            compare(text, base, true); // First compare the independent published algorithm, including L1/L2 at every scalar split.
            let (_directory, document) = fixture(text);
            let value = MarkdownBidiParagraph::analyze(
                document.text_snapshot(),
                0..document.utf16_len(),
                base,
                BidiMemoryBudget::default(),
            )
            .unwrap();
            assert!(matches!(value.resolution, Resolution::UniformZero(_)));
            let profile = value.storage_profile();
            assert_eq!((profile.cache_bytes, profile.page_loads), (0, 0));
            assert!(profile.source_borrow_max_bytes <= 4096);
        }
    }
}

#[test]
fn internal_multiple_b_and_complex_classes_preserve_exact_uba_and_cross_paragraph_rejection() {
    // Multiple actual B paragraphs may share a zero-level certificate but cannot become one soft line; complex classes still require complete paged UBA.
    for text in [
        "a\nb",
        "a\r\n",
        "a\u{0085}b",
        "\n\n",
        "a\u{2029}\u{2060}",
        "aא",
        "aع",
        "a\u{0661}",
        "a\u{202a}",
        "a\u{202b}",
        "a\u{202c}",
        "a\u{202d}",
        "a\u{202e}",
        "a\u{2066}",
        "a\u{2067}",
        "a\u{2068}",
        "a\u{2069}",
    ] {
        for base in [BidiBaseDirection::Auto, BidiBaseDirection::LeftToRight] {
            compare(text, base, true);
            let (_directory, document) = fixture(text);
            let mut value = MarkdownBidiParagraph::analyze(
                document.text_snapshot(),
                0..document.utf16_len(),
                base,
                BidiMemoryBudget::new(4 * store::PAGE_BYTES).unwrap(),
            )
            .unwrap();
            if matches!(
                text,
                "a\nb" | "a\r\n" | "a\u{0085}b" | "\n\n" | "a\u{2029}\u{2060}"
            ) {
                #[cfg(feature = "paged-shaping-frontier")]
                assert!(matches!(value.resolution, Resolution::UniformZero(_)));
                #[cfg(not(feature = "paged-shaping-frontier"))]
                assert!(matches!(value.resolution, Resolution::Paged(_)));
                assert!(matches!(
                    value.visit_visual_runs(0..document.utf16_len(), |_| {}),
                    Err(BidiError::InvalidRange)
                )); // All-zero levels do not authorize a line crossing B; CR/LF and NEL retain their original paragraph semantics.
            } else {
                assert!(matches!(value.resolution, Resolution::Paged(_)));
                // Actual RTL/AN/explicit controls/isolates still run full UBA; compare above checks every level and actual visual order.
            }
        }
    }
    let (_directory, document) = fixture("123中文");
    let value = MarkdownBidiParagraph::analyze(
        document.text_snapshot(),
        0..document.utf16_len(),
        BidiBaseDirection::RightToLeft,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    assert!(matches!(value.resolution, Resolution::Paged(_)));
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn uniform_zero_keeps_original_source_global_utf16_and_surrogate_boundary_contract() {
    // A Uniform certificate retains the same original snapshot and absolute coordinates, while range operations still reject surrogate interiors.
    let (_directory, document) = fixture("leadא\nA😀e\u{0301}12\nNEXT");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let foreign = crate::MarkdownLayoutTextSource::capture(&document).authority();
    let observer = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let range = 6..14;
    let mut value = MarkdownBidiParagraph::analyze_layout_source(
        source.clone(),
        &authority,
        range.clone(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
    )
    .unwrap();
    assert!(matches!(value.resolution, Resolution::UniformZero(_)));
    drop(document);
    assert_eq!(value.source_range(), range);
    assert_eq!(value.level_at_utf16(7).unwrap(), 0);
    assert_eq!(value.level_at_utf16(8).unwrap(), 0); // Both UTF16 head/low records retain their actual zero-level fact.
    for query in [8..9, 7..8, 8..8] {
        assert!(matches!(
            value.visit_logical_runs(query.clone(), |_| {}),
            Err(BidiError::InvalidScalarBoundary)
        ));
        assert!(matches!(
            value.visit_visual_runs(query, |_| {}),
            Err(BidiError::InvalidScalarBoundary)
        ));
    }
    let mut logical = Vec::new();
    value
        .visit_logical_runs(range.clone(), |run| logical.push(run))
        .unwrap();
    assert_eq!(
        logical,
        [MarkdownBidiRun {
            source_utf16_start: 6,
            source_utf16_end: 14,
            level: 0,
        }]
    );
    let mut empty_callbacks = 0;
    value
        .visit_visual_runs(14..14, |_| empty_callbacks += 1)
        .unwrap();
    assert_eq!(empty_callbacks, 0);
    assert!(matches!(
        value.level_at_utf16(14),
        Err(BidiError::InvalidRange)
    ));
    assert!(matches!(
        value.require_layout_source(&foreign),
        Err(BidiError::SourceMismatch)
    ));
    assert!(!mapping.unmap_succeeded()); // Uniform still owns the same actual snapshot; do not claim Source release before its last owner returns.
    drop(value);
    assert!(!observer.payload_released());
    drop(source);
    assert!(observer.payload_released());
    assert!(mapping.unmap_succeeded());
    assert_eq!(mapping.mapped_bytes(), 0);
    assert!(!mapping.failed());
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn uniform_zero_ready_and_last_callback_cancellation_remain_permanent() {
    // Uniform's single real run still obeys final-callback revocation for both logical and visual access; zero metadata does not bypass authority.
    let (_directory, document) = fixture("中文😀123");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let end = source.utf16_len();
    for visual in [false, true] {
        let cancellation = MarkdownBidiCancellation::new();
        let mut value = MarkdownBidiParagraph::analyze_layout_source_cancellable(
            source.clone(),
            &authority,
            0..end,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
            &cancellation,
        )
        .unwrap();
        assert!(matches!(value.resolution, Resolution::UniformZero(_)));
        assert_eq!(value.level_at_utf16(0).unwrap(), 0);
        let mut callbacks = 0;
        let result = if visual {
            value.visit_visual_runs(0..end, |_| {
                callbacks += 1;
                cancellation.cancel();
            })
        } else {
            value.visit_logical_runs(0..end, |_| {
                callbacks += 1;
                cancellation.cancel();
            })
        };
        assert_eq!(callbacks, 1);
        assert!(matches!(result, Err(BidiError::Cancelled)));
        assert!(matches!(value.level_at_utf16(0), Err(BidiError::Cancelled)));
        assert!(matches!(
            value.visit_visual_runs(0..0, |_| panic!("取消后不交付候选")),
            Err(BidiError::Cancelled)
        ));
        assert!(matches!(
            value.require_layout_source(&authority),
            Err(BidiError::Cancelled)
        ));
    }
}

#[cfg(feature = "paged-layout-breaks")]
#[test]
fn uniform_zero_negative_certificate_and_cached_queries_never_hide_source_poison() {
    // A negative eligibility result cannot restart Paged analysis over poisoned Source, and positive cached Uniform levels keep the same Source-first gate.
    for text in ["纯LTR中文😀123", "aא", "a\nb"] {
        let (_directory, document) = fixture(text);
        poison_original_read_windows(&document);
        let result = MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..document.utf16_len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
        );
        assert!(matches!(
            result,
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
            ))
        )); // Even an otherwise early negative certificate cannot rerun Paged work over actual failed Source and repackage it as Ready.
    }
    let (_directory, document) = fixture("纯LTR中文😀123");
    let source = crate::MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let cancellation = MarkdownBidiCancellation::new();
    let mut value = MarkdownBidiParagraph::analyze_layout_source_cancellable(
        source.clone(),
        &authority,
        0..source.utf16_len(),
        BidiBaseDirection::Auto,
        BidiMemoryBudget::default(),
        &cancellation,
    )
    .unwrap();
    assert!(matches!(value.resolution, Resolution::UniformZero(_)));
    value.level_at_utf16(0).unwrap();
    poison_original_read_windows(&document);
    assert!(source.visit_byte_range(0..1, |_| Ok(())).is_err());
    cancellation.cancel();
    assert!(matches!(
        value.level_at_utf16(0),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    )); // Known Source failure precedes cancellation and a cached Uniform zero level, retaining the actual errno type.
    assert!(value.visit_logical_runs(0..0, |_| {}).is_err());
    assert!(value.require_layout_source(&authority).is_err());
}

#[test]
#[ignore = "需显式指定OMNI_BIDI_CHARACTER_TEST；只跑官方中前256个合格纯LTR正例"]
fn official_uniform_zero_filtered_prefix_matches_declared_levels_and_order() {
    // This ignored entry explicitly selects at most the first 256 eligible official positives; it does not execute or certify the whole corpus.
    use std::io::BufRead;
    let path = std::env::var_os("OMNI_BIDI_CHARACTER_TEST").expect("必须显式指定官方数据路径");
    let file = std::fs::File::open(path).unwrap();
    let mut selected = 0;
    let mut corpus_version = None;
    for (line_number, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.unwrap();
        if let Some(version) = line
            .strip_prefix("# BidiCharacterTest-")
            .and_then(|value| value.strip_suffix(".txt"))
        {
            corpus_version = Some(version.to_owned());
        }
        let data = line.split('#').next().unwrap().trim();
        if data.is_empty() {
            continue;
        }
        let fields: Vec<_> = data.split(';').map(str::trim).collect();
        assert_eq!(fields.len(), 5);
        let base = match fields[1] {
            "0" => BidiBaseDirection::LeftToRight,
            "2" => BidiBaseDirection::Auto,
            _ => continue,
        };
        // Complete text belongs only to the explicitly finite official oracle; the product certificate does not use this String path.
        let source: String = fields[0]
            .split_whitespace()
            .map(|word| char::from_u32(u32::from_str_radix(word, 16).unwrap()).unwrap())
            .collect();
        assert!(source.chars().count() <= 4096);
        let mut terminal_b = false;
        let eligible = source.chars().all(|scalar| {
            let class = UnicodeBidiData.bidi_class(scalar);
            let allowed = !terminal_b && uniform::eligible_class(class);
            terminal_b = class == BidiClass::B;
            allowed
        });
        if !eligible {
            continue;
        }
        let (_directory, document) = fixture(&source);
        let mut value = MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..document.utf16_len(),
            base,
            BidiMemoryBudget::default(),
        )
        .unwrap();
        assert!(matches!(value.resolution, Resolution::UniformZero(_)));
        assert_eq!(fields[2], "0");
        let expected: Vec<_> = fields[3].split_whitespace().collect();
        let mut at = 0;
        let mut offsets = vec![0];
        for (scalar_index, scalar) in source.chars().enumerate() {
            for unit in at..at + scalar.len_utf16() {
                assert_eq!(value.level_at_utf16(unit).unwrap(), 0);
            }
            assert!(matches!(expected[scalar_index], "0" | "x"));
            at += scalar.len_utf16();
            offsets.push(at);
        }
        assert_eq!(offsets.len() - 1, expected.len());
        let mut actual_order = Vec::new();
        value
            .visit_visual_runs(0..at, |run| {
                let first = offsets.binary_search(&run.source_utf16_start).unwrap();
                let last = offsets.binary_search(&run.source_utf16_end).unwrap();
                assert_eq!(run.level, 0);
                for (index, class) in expected.iter().enumerate().take(last).skip(first) {
                    if *class != "x" {
                        actual_order.push(index);
                    }
                }
            })
            .unwrap(); // Order must come from actual visual-query callbacks, not input order substituted for the API result.
        let expected_order: Vec<usize> = fields[4]
            .split_whitespace()
            .map(|word| word.parse().unwrap())
            .collect();
        assert_eq!(actual_order, expected_order, "官方行{}", line_number + 1);
        selected += 1;
        if selected == 256 {
            break; // This is an explicitly finite filtered prefix, not a passing result for the entire official corpus.
        }
    }
    assert_eq!(selected, 256);
    assert_eq!(corpus_version.as_deref(), Some("18.0.0"));
    eprintln!("bidi_uniform_official_filtered_prefix cases={selected} properties=18.0.0");
}

#[test]
fn uniform_zero_requires_real_end_of_large_unicode_source_before_committing() {
    // Write a large fixture incrementally and place a single complex scalar at its real tail; no eligible prefix may certify the unread successor.
    use std::io::Write;
    for complex_tail in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("uniform-large.md");
        let mut file = std::fs::File::create(&path).unwrap();
        let chunk = [b'a'; 4093];
        for _ in 0..64 {
            file.write_all(&chunk).unwrap();
            file.write_all("😀\u{0301}".as_bytes()).unwrap();
        }
        if complex_tail {
            file.write_all("א".as_bytes()).unwrap();
        }
        drop(file);
        let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
        let mut value = MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..document.utf16_len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(4 * store::PAGE_BYTES).unwrap(),
        )
        .unwrap();
        assert!(value.storage_profile().source_borrow_max_bytes <= 4096);
        if complex_tail {
            assert!(matches!(value.resolution, Resolution::Paged(_)));
            assert_eq!(value.level_at_utf16(document.utf16_len() - 1).unwrap(), 1);
        } else {
            assert!(matches!(value.resolution, Resolution::UniformZero(_)));
            assert_eq!(value.storage_profile().cache_bytes, 0);
            assert_eq!(value.level_at_utf16(document.utf16_len() - 1).unwrap(), 0);
        } // The sole complex character lies before actual EOF; many consumed 4-KiB prefixes do not establish a complete certificate.
    }
}

#[test]
fn paragraph_base_preserves_rtl_ls_context_and_is_not_the_first_glyph_level() {
    // Paragraph base comes from actual P2/P3 context and differs from a glyph's resolved level; compare scalar endpoints and real EOF with the finite oracle.
    for text in ["abc\u{2028}", "אב\u{2028}", "123 אב\u{2028}", "אב\r\n", ""] {
        let (_directory, document) = fixture(text);
        let units: Vec<u16> = text.encode_utf16().collect();
        let oracle = BidiInfo::new_with_data_source(&UnicodeBidiData, &units, None);
        let mut actual = MarkdownBidiParagraph::analyze(
            document.text_snapshot(),
            0..units.len(),
            BidiBaseDirection::Auto,
            BidiMemoryBudget::default(),
        )
        .unwrap();
        let mut at = 0;
        for scalar in text.chars() {
            let expected = oracle
                .paragraphs
                .iter()
                .find(|paragraph| paragraph.range.contains(&at))
                .unwrap()
                .level
                .number();
            assert_eq!(
                actual.paragraph_base_level_at_utf16(at).unwrap(),
                Some(expected)
            );
            if scalar.len_utf16() == 2 {
                assert!(matches!(
                    actual.paragraph_base_level_at_utf16(at + 1),
                    Err(BidiError::InvalidScalarBoundary)
                ));
            }
            at += scalar.len_utf16();
        }
        assert_eq!(
            actual.paragraph_base_level_at_utf16(units.len()).unwrap(),
            None
        );
        assert!(matches!(
            actual.paragraph_base_level_at_utf16(units.len() + 1),
            Err(BidiError::InvalidRange)
        ));
    }
}
