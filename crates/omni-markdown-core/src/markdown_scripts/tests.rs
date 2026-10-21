//! Complete strings and vectors are confined to these finite synthetic oracles.
//! Production retains fixed cluster summaries and paginated metadata instead.
use super::*;
use crate::MarkdownDocumentBuffer;

#[cfg(feature = "paged-line-compose")]
#[path = "fold_tests.rs"]
mod fold_tests;

/// Leases an actual source page before using the existing reclaim fault hook.
/// Returning a pin alone does not imply that the backing performed an msync.
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
    ); // The reclaim path must report the injected EIO before later queries observe its sticky source error.
}

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    // Each case owns its file and directory. The document uses the real backing
    // rather than a replacement source with predeclared script properties.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("scripts.md");
    std::fs::write(&path, text).unwrap();
    (
        directory,
        MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap(),
    )
}
fn make_job(
    text: &str,
) -> (
    tempfile::TempDir,
    MarkdownDocumentBuffer,
    MarkdownLayoutTextAuthority,
    MarkdownScriptJob,
) {
    let (directory, document) = fixture(text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    // Rule, quota, and failure cases retain scalar scanning. Summary tests use
    // this same original machine as their separate differential oracle.
    let job =
        MarkdownScriptJob::new_streaming(source, &authority, MarkdownScriptCancellation::new())
            .unwrap();
    (directory, document, authority, job)
}
fn finish(
    job: &mut MarkdownScriptJob,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownScriptBudget,
) {
    // Every Pending quantum must perform real bounded work. The finite loop
    // limit catches lack of progress without authorizing a partial result.
    for _ in 0..1_000_000 {
        let step = job.step(authority, budget).unwrap();
        assert!(
            step.bytes <= budget.bytes
                && step.scalars <= budget.scalars
                && step.records <= budget.records
                && step.stack_ops <= budget.stack_ops
        );
        if step.status == MarkdownScriptStatus::Ready {
            return;
        }
        assert!(
            step.bytes + step.scalars + step.records + step.stack_ops > 0,
            "Pending必须保留实际有限进展，不能自旋等重签"
        );
    }
    panic!("有限合成样本未完成");
}
fn runs(
    job: &mut MarkdownScriptJob,
    authority: &MarkdownLayoutTextAuthority,
) -> Vec<MarkdownScriptRun> {
    // Inspect the stored records only to enumerate this finite oracle, then
    // verify each through the public query using the original source authority.
    (0..job.store.run_count())
        .map(|index| {
            let value = job.store.run(index).unwrap();
            assert_eq!(
                job.run_at(authority, MarkdownScriptUnit::Byte, value.start_byte)
                    .unwrap(),
                MarkdownScriptAnswer::Known(Some(value))
            );
            value
        })
        .collect()
}
fn tagged(text: &str, budget: MarkdownScriptBudget) -> Vec<(String, [u8; 4])> {
    // Copying completed fixture slices is confined to expected test output;
    // production does not materialize the document to obtain script labels.
    let (_directory, _document, authority, mut job) = make_job(text);
    finish(&mut job, &authority, budget);
    runs(&mut job, &authority)
        .into_iter()
        .map(|run| {
            (
                text[run.start_byte..run.end_byte].to_owned(),
                run.script.iso15924_tag(),
            )
        })
        .collect()
}

#[test]
fn scx_common_inherited_marks_and_unknown_follow_declared_cluster_policy() {
    // Literal tags distinguish the declared SCX/cluster policy, including
    // inherited marks, context-sensitive extensions, Common, and Unknown.
    for (text, expected) in [
        ("hello 世界", vec![("hello ", *b"Latn"), ("世界", *b"Hani")]),
        ("aーあ", vec![("a", *b"Latn"), ("ーあ", *b"Hira")]),
        ("\u{11800}\u{0966}", vec![("\u{11800}\u{0966}", *b"Dogr")]),
        ("a\u{05b0}", vec![("a\u{05b0}", *b"Latn")]),
        ("a\u{102b}", vec![("a\u{102b}", *b"Latn")]),
        ("◌\u{05b0}", vec![("◌\u{05b0}", *b"Hebr")]),
        ("🙂‍👩🏽", vec![("🙂‍👩🏽", *b"Zyyy")]),
        (
            "a\u{e000}b",
            vec![("a", *b"Latn"), ("\u{e000}", *b"Zzzz"), ("b", *b"Latn")],
        ),
    ] {
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(text, tag)| (text.to_owned(), tag))
            .collect();
        assert_eq!(
            tagged(text, MarkdownScriptBudget::default()),
            expected,
            "{text:?}"
        );
        // Byte-at-a-time decoding must preserve the same full-source context as
        // the ordinary quota, including scalars split across source visits.
        assert_eq!(
            tagged(
                text,
                MarkdownScriptBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                    stack_ops: 1
                }
            ),
            expected,
            "逐byte分块不能冒充sot/eot {text:?}"
        );
    }
}

#[test]
fn nested_pairs_keep_outer_context_beyond_32_and_paragraphs_reset_only_at_real_breaks() {
    // A closing pair inherits its actual outer script, whereas unmatched
    // leading brackets can resolve with the script found inside them.
    assert_eq!(
        tagged("A (α) B", MarkdownScriptBudget::default()),
        vec![
            ("A (".into(), *b"Latn"),
            ("α".into(), *b"Grek"),
            (") B".into(), *b"Latn")
        ]
    );
    assert_eq!(
        tagged("(α)", MarkdownScriptBudget::default()),
        vec![("(α)".into(), *b"Grek")]
    );
    let text = format!("a({}α{}א)", "(".repeat(96), ")".repeat(96));
    // Resume real frame operations under a one-operation quota; the nesting
    // exceeds a small in-memory stack and still retains the original opener.
    let (_directory, _document, authority, mut job) = make_job(&text);
    finish(
        &mut job,
        &authority,
        MarkdownScriptBudget {
            bytes: 3,
            scalars: 2,
            records: 1,
            stack_ops: 1,
        },
    );
    assert!(job.store.frame_count() > 32);
    let last = job
        .run_at(&authority, MarkdownScriptUnit::Byte, text.len() - 1)
        .unwrap();
    assert!(
        matches!(last, MarkdownScriptAnswer::Known(Some(run)) if run.script.iso15924_tag() == *b"Latn")
    );
    // The last closer refers to an opener beneath 96 frames. Dropping older
    // frames would incorrectly let the intervening Hebrew context own it.
    let reset = tagged("a(α\r\nא)", MarkdownScriptBudget::default());
    assert_eq!(reset.last().unwrap().1, *b"Hebr");
    assert!(!reset.iter().any(|(part, _)| part.starts_with('\n'))); // Script itemization must not split the original CRLF grapheme.
    let quotes = tagged("a'α'א", MarkdownScriptBudget::default());
    assert_eq!(quotes[1], ("α'".into(), *b"Grek")); // A language-dependent quote is not automatically a paired bracket.
}

#[test]
fn pending_prefix_and_every_byte_split_preserve_global_utf16_coverage() {
    // A partial first scalar is not a published run or proof of absence. The
    // subsequent sweep exercises every byte quota against the same full oracle.
    let text = "  a😀\u{0301}世界(א)";
    let (_directory, _document, authority, mut pending) = make_job(text);
    assert_eq!(
        pending
            .run_at(&authority, MarkdownScriptUnit::Utf16, 0)
            .unwrap(),
        MarkdownScriptAnswer::Pending
    );
    pending
        .step(
            &authority,
            MarkdownScriptBudget {
                bytes: 1,
                scalars: 1,
                records: 1,
                stack_ops: 1,
            },
        )
        .unwrap();
    assert_eq!(
        pending
            .run_at(&authority, MarkdownScriptUnit::Byte, 0)
            .unwrap(),
        MarkdownScriptAnswer::Pending
    );
    let expected = tagged(text, MarkdownScriptBudget::default());
    for bytes in 1..=text.len() {
        let (_directory, _document, authority, mut job) = make_job(text);
        finish(
            &mut job,
            &authority,
            MarkdownScriptBudget {
                bytes,
                scalars: 1,
                records: 1,
                stack_ops: 1,
            },
        );
        let actual: Vec<_> = runs(&mut job, &authority)
            .into_iter()
            .map(|run| {
                assert_eq!(
                    text[..run.start_byte].encode_utf16().count(),
                    run.start_utf16
                );
                assert_eq!(text[..run.end_byte].encode_utf16().count(), run.end_utf16);
                (
                    text[run.start_byte..run.end_byte].to_owned(),
                    run.script.iso15924_tag(),
                )
            })
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(
            job.run_at(&authority, MarkdownScriptUnit::Byte, text.len())
                .unwrap(),
            MarkdownScriptAnswer::Known(None)
        );
        assert_eq!(
            job.run_at(
                &authority,
                MarkdownScriptUnit::Utf16,
                text.encode_utf16().count()
            )
            .unwrap(),
            MarkdownScriptAnswer::Known(None)
        );
    }
}

#[test]
fn cross_page_run_queries_keep_exact_unicode_ranges_and_unfinished_frontier() {
    // Three specific scripts alternate scalar by scalar. Deseret occupies four
    // UTF-8 bytes and two UTF-16 units, so neither coordinate is a record ordinal.
    // The 1,029 actual runs cross four full append pages and leave five tail records.
    let text = "a𐐀中".repeat(343);
    let (directory, document) = fixture(&text);
    let mapping = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mut job =
        MarkdownScriptJob::new(source, &authority, MarkdownScriptCancellation::new()).unwrap();
    let observer = job.resource_observer();
    let budget = MarkdownScriptBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        stack_ops: 1,
    };
    // Expected extents come from the literal scalars and public script tags,
    // without reading Store state or mutating pagination to prepare the answer.
    let segments = [
        (0..1, 0..1, *b"Latn"),
        (1..5, 1..3, *b"Dsrt"),
        (5..8, 3..4, *b"Hani"),
    ];
    let assert_run = |job: &mut MarkdownScriptJob, ordinal: usize| {
        let group = ordinal / 3;
        let (bytes, utf16, tag) = &segments[ordinal % 3];
        let bytes = group * 8 + bytes.start..group * 8 + bytes.end;
        let utf16 = group * 4 + utf16.start..group * 4 + utf16.end;
        // First and last units belong to the same complete run. A UTF-16 unit
        // inside a surrogate pair must not invent another script boundary.
        for (unit, offset) in [
            (MarkdownScriptUnit::Byte, bytes.start),
            (MarkdownScriptUnit::Byte, bytes.end - 1),
            (MarkdownScriptUnit::Utf16, utf16.start),
            (MarkdownScriptUnit::Utf16, utf16.end - 1),
        ] {
            let MarkdownScriptAnswer::Known(Some(run)) =
                job.run_at(&authority, unit, offset).unwrap()
            else {
                panic!("已完成的真实脚本范围必须可查询");
            };
            assert_eq!(run.start_byte..run.end_byte, bytes);
            assert_eq!(run.start_utf16..run.end_utf16, utf16);
            assert_eq!(run.script.iso15924_tag(), *tag);
            #[cfg(feature = "paged-shaping-frontier")]
            {
                // The frontier borrows this same closed run. Its borrow ends
                // before further scanning or another mutable query can proceed.
                let MarkdownScriptPrefixAnswer::Known(Some(prefix)) =
                    job.stable_prefix_at(&authority, unit, offset).unwrap()
                else {
                    panic!("已完成run不能退成开放脚本前缀");
                };
                assert_eq!(prefix.facts().unwrap(), run);
                assert_eq!(prefix.kind().unwrap(), MarkdownScriptPrefixKind::ClosedRun);
            }
        }
    };
    for ordinal in [255, 256, 511, 512, 1023, 1024] {
        let group = ordinal / 3;
        let (bytes, utf16, _) = &segments[ordinal % 3];
        let end_byte = group * 8 + bytes.end;
        let end_utf16 = group * 4 + utf16.end;
        // Advance through public one-byte steps until this run is published.
        // Queries must not scan its successor; records=1 also exposes the exact
        // publication frontier without a second record hiding it in one step.
        let mut closed = false;
        for _ in 0..text.len() * 8 {
            if matches!(
                job.run_at(&authority, MarkdownScriptUnit::Byte, end_byte - 1)
                    .unwrap(),
                MarkdownScriptAnswer::Known(Some(_))
            ) {
                closed = true;
                break;
            }
            let step = job.step(&authority, budget).unwrap();
            assert!(step.bytes <= 1 && step.scalars <= 1);
            assert!(step.records <= 1 && step.stack_ops <= 1);
            assert_eq!(step.status, MarkdownScriptStatus::Pending);
        }
        assert!(closed, "有限原输入必须能到达这个真实发布边界");
        // Query both sides of a page edge and older file records. An equal end
        // belongs to the next run, which remains Pending until actually closed.
        for known in [0, ordinal - 1, ordinal] {
            assert_run(&mut job, known);
        }
        for (unit, offset) in [
            (MarkdownScriptUnit::Byte, end_byte),
            (MarkdownScriptUnit::Utf16, end_utf16),
            (MarkdownScriptUnit::Byte, text.len()),
            (MarkdownScriptUnit::Utf16, text.encode_utf16().count()),
        ] {
            assert_eq!(
                job.run_at(&authority, unit, offset).unwrap(),
                MarkdownScriptAnswer::Pending
            );
        }
    }
    // After real EOF, reverse queries revisit the tail, old page ends, and page
    // starts. Only completion can establish absence at the source endpoint.
    finish(&mut job, &authority, budget);
    for ordinal in [1028, 0, 1024, 1023, 512, 511, 256, 255, 1] {
        assert_run(&mut job, ordinal);
    }
    for (unit, offset) in [
        (MarkdownScriptUnit::Byte, text.len()),
        (MarkdownScriptUnit::Utf16, text.encode_utf16().count()),
    ] {
        assert_eq!(
            job.run_at(&authority, unit, offset).unwrap(),
            MarkdownScriptAnswer::Known(None)
        );
    }
    assert_eq!(
        std::fs::read(directory.path().join("scripts.md")).unwrap(),
        text.as_bytes()
    );
    assert_eq!(document.generation(), 0);
    assert!(!document.can_undo() && !document.can_redo());
    // Closing relinquishes both indexes and the source. Weak resource observers
    // check actual release instead of treating a closed flag as deallocation.
    job.close().unwrap();
    drop(document);
    let facts = observer.snapshot();
    assert!(!facts.run_file_open && !facts.stack_file_open);
    assert_eq!(facts.buffer_bytes, 0);
    assert_eq!(facts.mapped_bytes, 0);
    assert!(!facts.cleanup_failed && facts.source_payload_released);
    assert!(mapping.unmap_succeeded() && !mapping.failed());
}

#[test]
fn sealed_run_queries_keep_foreign_source_cancel_and_invalid_offset_errors() {
    // Each case first publishes multiple real metadata pages through the public
    // entry. Revocation then covers both old and recent records, without private
    // record mutation, a substitute reader, or an assumed cache hit.
    enum Failure {
        Cancelled,
        ForeignSource,
        InvalidOffset,
    }
    let text = "a𐐀中".repeat(343);
    for failure in [
        Failure::Cancelled,
        Failure::ForeignSource,
        Failure::InvalidOffset,
    ] {
        let (_directory, document) = fixture(&text);
        let mapping = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let cancellation = MarkdownScriptCancellation::new();
        let mut job = MarkdownScriptJob::new(source, &authority, cancellation.clone()).unwrap();
        let observer = job.resource_observer();
        finish(&mut job, &authority, MarkdownScriptBudget::default());
        match failure {
            Failure::Cancelled => {
                cancellation.cancel();
                // Both the append tail and older FD region must observe the
                // same cancellation before an existing record can be returned.
                for offset in [text.len() - 1, 0] {
                    assert!(matches!(
                        job.run_at(&authority, MarkdownScriptUnit::Byte, offset),
                        Err(MarkdownScriptError::Cancelled)
                    ));
                }
            }
            Failure::ForeignSource => {
                // Equal bytes in another document do not share source authority;
                // the mismatched query also permanently poisons this job.
                let (_other_directory, other_document) = fixture(&text);
                let foreign = MarkdownLayoutTextSource::capture(&other_document).authority();
                assert!(matches!(
                    job.run_at(&foreign, MarkdownScriptUnit::Utf16, 0),
                    Err(MarkdownScriptError::SourceMismatch)
                ));
                assert!(matches!(
                    job.run_at(&authority, MarkdownScriptUnit::Byte, text.len() - 1),
                    Err(MarkdownScriptError::Poisoned)
                ));
            }
            Failure::InvalidOffset => {
                // An out-of-range public query revokes subsequent cached reads
                // instead of converting its invalid endpoint into Known(None).
                assert!(matches!(
                    job.run_at(&authority, MarkdownScriptUnit::Byte, text.len() + 1),
                    Err(MarkdownScriptError::InvalidRange)
                ));
                assert!(matches!(
                    job.run_at(&authority, MarkdownScriptUnit::Byte, 0),
                    Err(MarkdownScriptError::Poisoned)
                ));
            }
        }
        // Revocation does not prevent the original owner from cleaning up. The
        // source and anonymous indexes must also end after an error.
        job.close().unwrap();
        drop(document);
        let facts = observer.snapshot();
        assert!(!facts.run_file_open && !facts.stack_file_open);
        assert_eq!(facts.buffer_bytes, 0);
        assert_eq!(facts.mapped_bytes, 0);
        assert!(!facts.cleanup_failed && facts.source_payload_released);
        assert!(mapping.unmap_succeeded() && !mapping.failed());
    }
}

#[test]
fn authority_cancel_source_failure_and_real_release_are_not_replaced_by_cached_runs() {
    // Establish a real completed answer before injecting the existing reclaim
    // failure. Cached metadata and cancellation must not hide the source errno.
    let (directory, document) = fixture("a(א)世界");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let cancellation = MarkdownScriptCancellation::new();
    let mut job = MarkdownScriptJob::new(source.clone(), &authority, cancellation.clone()).unwrap();
    let observer = job.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    finish(&mut job, &authority, MarkdownScriptBudget::default());
    assert!(matches!(
        job.run_at(&authority, MarkdownScriptUnit::Utf16, 0)
            .unwrap(),
        MarkdownScriptAnswer::Known(Some(_))
    ));
    let facts = observer.snapshot();
    assert!(!facts.stack_file_open);
    assert!(
        facts.run_peak_mapped_bytes == 0
            && facts.stack_peak_mapped_bytes == 0
            && facts.mapped_bytes == 0
            && facts.run_peak_buffer_bytes <= 16 * 1024
            && facts.stack_peak_buffer_bytes <= 16 * 1024
            && facts.buffer_bytes <= MARKDOWN_SCRIPT_INDEX_BYTES
    );
    poison_original_read_windows(&document);
    assert!(source.visit_byte_range(0..1, |_| Ok(())).is_err());
    cancellation.cancel();
    assert!(matches!(
        job.run_at(&authority, MarkdownScriptUnit::Byte, 0),
        Err(MarkdownScriptError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(_)
        ))
    ));
    drop(document);
    drop(source);
    drop(job);
    let facts = observer.snapshot();
    assert!(
        !facts.run_file_open
            && !facts.stack_file_open
            && facts.mapped_bytes == 0
            && facts.buffer_bytes == 0
            && !facts.cleanup_failed
            && facts.source_payload_released
    );
    assert!(mapping.unmap_succeeded() && mapping.failed()); // Successful unmapping must not erase the earlier injected reclaim failure.
    drop(directory);
    // A separate healthy source isolates identity rejection from the preceding
    // sticky I/O failure and verifies that a later valid ticket cannot revive it.
    let (_directory, _document, authority, mut job) = make_job("abc");
    let (_other_directory, other_document) = fixture("abc");
    let foreign = MarkdownLayoutTextSource::capture(&other_document).authority();
    assert!(matches!(
        job.step(&foreign, MarkdownScriptBudget::default()),
        Err(MarkdownScriptError::SourceMismatch)
    ));
    assert!(matches!(
        job.step(&authority, MarkdownScriptBudget::default()),
        Err(MarkdownScriptError::Poisoned)
    ));
}

#[test]
fn background_cancellation_joins_before_actual_metadata_and_source_release_receipts() {
    use std::sync::{Barrier, mpsc};
    let (_directory, document) = fixture("a(א)中文");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mapping = document.original_backing_observer().unwrap();
    let cancellation = MarkdownScriptCancellation::new();
    let worker_cancellation = cancellation.clone();
    let entered = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let worker_entered = entered.clone();
    let worker_resume = resume.clone();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut job = MarkdownScriptJob::new(source, &authority, worker_cancellation).unwrap();
        send.send(job.resource_observer()).unwrap();
        worker_entered.wait();
        worker_resume.wait();
        assert!(matches!(
            job.step(&authority, MarkdownScriptBudget::default()),
            Err(MarkdownScriptError::Cancelled)
        ));
    });
    let observer = receive.recv().unwrap();
    entered.wait();
    drop(document);
    cancellation.cancel();
    // No records have advanced, so there is no metadata FD. The worker still
    // owns the live source; cancellation alone cannot count as its release.
    assert!(!observer.snapshot().run_file_open && !mapping.unmap_succeeded());
    assert_eq!(observer.snapshot().buffer_bytes, 0);
    resume.wait();
    worker.join().unwrap();
    let facts = observer.snapshot();
    assert!(
        !facts.run_file_open
            && !facts.stack_file_open
            && facts.mapped_bytes == 0
            && facts.buffer_bytes == 0
            && !facts.cleanup_failed
            && facts.source_payload_released
    );
    assert!(mapping.unmap_succeeded() && !mapping.failed()); // Joining the cancelled worker establishes clean mapping release without an I/O failure.
}

#[test]
fn unicode18_generated_scx_sets_and_bracket_cluster_assumptions_match_exact_tables() {
    // These checks describe the generated Unicode tables and assumptions used
    // by the machine; they are not evidence of a complete native UI workflow.
    for &(first, last, _) in unicode_data::EXTENSIONS {
        for point in first..=last {
            let p = properties::get(char::from_u32(point).unwrap());
            let set = p.scripts.unwrap();
            if !matches!(p.primary, unicode_data::COMMON | unicode_data::INHERITED) {
                assert!(set.contains(p.primary));
            }
        }
    }
    for &(point, _, _) in unicode_data::BRACKETS {
        // The actual GCB cursor must put a bracket outside the neighboring base
        // clusters; the script stack relies on that generated-data property.
        let scalar = char::from_u32(point).unwrap();
        assert!(!properties::get(scalar).mark);
        let mut cursor = crate::markdown_graphemes::MarkdownGraphemeScalarCursor::default();
        assert!(cursor.push('a'));
        assert!(cursor.push(scalar));
        assert!(cursor.push('a'));
    }
    assert_eq!(unicode_data::UNICODE_VERSION, (18, 0, 0));
    assert_eq!(unicode_data::SETS.len(), 120);
    assert_eq!(unicode_data::BRACKETS.len(), 130);
}

#[test]
fn empty_invalid_budget_ready_cancel_and_real_metadata_io_failure_never_forge_known() {
    // Empty input reaches real EOF, whereas invalid quotas poison a nonempty
    // job. Neither a stalled first step nor an error can fabricate Known(None).
    let (_directory, _document, authority, mut empty) = make_job("");
    finish(
        &mut empty,
        &authority,
        MarkdownScriptBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            stack_ops: 1,
        },
    );
    assert_eq!(empty.store.run_count(), 0);
    assert_eq!(
        empty
            .run_at(&authority, MarkdownScriptUnit::Utf16, 0)
            .unwrap(),
        MarkdownScriptAnswer::Known(None)
    );
    let (_directory, _document, authority, mut invalid) = make_job("abc");
    assert!(matches!(
        invalid.step(
            &authority,
            MarkdownScriptBudget {
                bytes: 0,
                ..MarkdownScriptBudget::default()
            }
        ),
        Err(MarkdownScriptError::InvalidBudget)
    ));
    assert!(matches!(
        invalid.run_at(&authority, MarkdownScriptUnit::Byte, 0),
        Err(MarkdownScriptError::Poisoned)
    ));
    let (_directory, _document, authority, mut ready) = make_job("abc");
    // Ready is a semantic state, not an exemption from current cancellation.
    finish(&mut ready, &authority, MarkdownScriptBudget::default());
    ready.cancellation.cancel();
    for _ in 0..2 {
        assert!(matches!(
            ready.run_at(&authority, MarkdownScriptUnit::Byte, 0),
            Err(MarkdownScriptError::Cancelled)
        ));
    }
    let (directory, _document, authority, mut broken) = make_job(&"aא".repeat(300));
    // This existing private store injection installs a real read-only FD. The
    // eventual page flush must surface the kernel's EBADF rather than a test
    // implementation returning a predetermined storage error.
    let path = directory.path().join("readonly-index");
    std::fs::write(&path, []).unwrap();
    broken.store =
        store::Store::with_runs_file(std::fs::File::open(path).unwrap(), &broken.cancellation)
            .unwrap();
    let observer = broken.resource_observer();
    // The first write requires crossing 256 actual runs. A short private seal
    // needs no file consumer, so merely sealing a small fixture would not fail.
    let mut failure = None;
    for _ in 0..64 {
        match broken.step(&authority, MarkdownScriptBudget::default()) {
            Ok(_) => {}
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    assert!(
        matches!(failure, Some(MarkdownScriptError::Storage(error)) if error.raw_os_error() == Some(libc::EBADF))
    );
    assert!(matches!(
        broken.run_at(&authority, MarkdownScriptUnit::Byte, 0),
        Err(MarkdownScriptError::Poisoned)
    ));
    drop(broken);
    let facts = observer.snapshot();
    assert!(
        !facts.run_file_open
            && !facts.stack_file_open
            && facts.mapped_bytes == 0
            && facts.buffer_bytes == 0
    );
    assert!(facts.cleanup_failed);
}
