//! Verify product entry points with finite synthetic oracles. Official test data is read only
//! through an explicit environment path, never copied into the repository.
use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    rc::Rc,
};
use tempfile::TempDir;

#[cfg(feature = "paged-shaping-frontier")]
#[path = "fold_tests.rs"]
mod fold_tests;

const TINY: MarkdownGraphemeBudget = MarkdownGraphemeBudget {
    bytes: 3,
    scalars: 1,
    records: 1,
};
/// Borrow a real source page before failing the existing reclaim boundary. Returning a pin
/// alone does not establish that the actual reclaim operation ran.
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
    ); // Only after actual unpinned-page reclaim fails should consumers observe this permanent Source error.
}

fn fixture(bytes: &[u8]) -> (TempDir, MarkdownDocumentBuffer) {
    // Each fixture owns its isolated file and directory; no production vault is involved.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graphemes.md");
    std::fs::write(&path, bytes).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}
fn source(bytes: &[u8]) -> MarkdownLayoutTextSource {
    // The captured immutable payload survives the document and pathname owners. Algorithms
    // must not need the document, its history, or its save authority to read this source.
    let (_directory, document) = fixture(bytes);
    MarkdownLayoutTextSource::capture(&document)
}
fn finish(job: &mut MarkdownGraphemeJob, authority: &MarkdownLayoutTextAuthority) {
    let length = job.storage_profile().source_bytes;
    // Tiny independent quanta may suspend mid-scalar. Every unfinished step must consume
    // actual input or output records rather than completing through empty polling.
    for _ in 0..length.saturating_mul(4).saturating_add(16) {
        let step = job.step(authority, TINY).unwrap();
        assert!(
            step.bytes <= TINY.bytes
                && step.scalars <= TINY.scalars
                && step.records <= TINY.records
        );
        if step.status == MarkdownGraphemeStatus::Ready {
            assert!(job.is_ready());
            let profile = job.storage_profile();
            assert_eq!(
                profile.source_bytes_read,
                if profile.dense_printable_ascii {
                    0
                } else {
                    length
                }
            );
            return;
        }
        assert!(step.bytes > 0 || step.scalars > 0 || step.records > 0);
    }
    panic!("有限step未完成真实EOF");
}
fn released(observer: &MarkdownGraphemeResourceObserver, source_done: bool) {
    // Index cleanup and source-payload destruction are separate ownership facts. These
    // bounded metadata measurements do not claim whole-process RSS or unrelated owners.
    let facts = observer.snapshot();
    assert!(facts.metadata_owner_released);
    assert!(!facts.metadata_file_open);
    assert_eq!(facts.metadata_map_bytes, 0);
    assert_eq!(facts.metadata_peak_map_bytes, 0);
    assert_eq!(facts.metadata_buffer_bytes, 0);
    assert!(facts.metadata_peak_buffer_bytes <= MARKDOWN_GRAPHEME_INDEX_BYTES);
    assert!(!facts.cleanup_failed);
    assert_eq!(facts.source_payload_released, source_done);
}
fn coordinate(boundary: MarkdownGraphemeBoundary, unit: MarkdownGraphemeUnit) -> usize {
    match unit {
        MarkdownGraphemeUnit::Byte => boundary.byte,
        MarkdownGraphemeUnit::Utf16 => boundary.utf16,
    }
}
fn compare(
    job: &mut MarkdownGraphemeJob,
    authority: &MarkdownLayoutTextAuthority,
    expected: &[MarkdownGraphemeBoundary],
) -> usize {
    assert_eq!(job.storage_profile().boundary_count, expected.len());
    let mut count = 0;
    for unit in [MarkdownGraphemeUnit::Byte, MarkdownGraphemeUnit::Utf16] {
        let maximum = coordinate(*expected.last().unwrap(), unit);
        // Query every coordinate, including UTF-8 interiors and UTF-16 low surrogates.
        // Only independently supplied break positions may produce a matching boundary.
        for offset in 0..=maximum {
            for query in [
                MarkdownGraphemeQuery::IsBoundary,
                MarkdownGraphemeQuery::Previous,
                MarkdownGraphemeQuery::Next,
            ] {
                let wanted = match query {
                    MarkdownGraphemeQuery::IsBoundary => expected
                        .iter()
                        .copied()
                        .find(|&b| coordinate(b, unit) == offset),
                    MarkdownGraphemeQuery::Previous => expected
                        .iter()
                        .copied()
                        .rev()
                        .find(|&b| coordinate(b, unit) < offset),
                    MarkdownGraphemeQuery::Next => expected
                        .iter()
                        .copied()
                        .find(|&b| coordinate(b, unit) > offset),
                };
                assert_eq!(
                    job.query(authority, unit, offset, query).unwrap(),
                    MarkdownGraphemeAnswer::Known(wanted),
                    "unit={unit:?} offset={offset} query={query:?}"
                );
                count += 1;
            }
        }
        // Reverse expectations come from the independent break list, not the DFA's own forward output.
        for &boundary in expected.iter().rev() {
            let offset = coordinate(boundary, unit);
            let wanted = expected
                .iter()
                .copied()
                .rev()
                .find(|&b| coordinate(b, unit) < offset);
            assert_eq!(
                job.query(authority, unit, offset, MarkdownGraphemeQuery::Previous)
                    .unwrap(),
                MarkdownGraphemeAnswer::Known(wanted)
            );
            count += 1;
        }
    }
    count
}
fn each_split(bytes: &[u8], expected: &[MarkdownGraphemeBoundary]) {
    // Force the actual paged machine through every byte split of the finite source;
    // Dense admission must not bypass UTF-8, rule-state, or index I/O boundaries.
    let source = source(bytes);
    let authority = source.authority();
    for split in 0..=bytes.len() {
        let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
        job.split = Some(split);
        let observer = job.resource_observer();
        finish(&mut job, &authority);
        compare(&mut job, &authority, expected);
        job.close().unwrap();
        released(&observer, false);
    }
}

#[test]
fn empty_crlf_surrogates_and_unicode18_rules_preserve_exact_coordinates() {
    each_split(b"", &[MarkdownGraphemeBoundary { byte: 0, utf16: 0 }]);
    each_split(
        "🧭\r\n中".as_bytes(),
        &[
            MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
            MarkdownGraphemeBoundary { byte: 4, utf16: 2 },
            MarkdownGraphemeBoundary { byte: 6, utf16: 4 },
            MarkdownGraphemeBoundary { byte: 9, utf16: 5 },
        ],
    );
    // Unicode 18 GB9c no longer requires a preceding Consonant, and U+1F1AE is not EP.
    // Keep both explicit expectations independent of the earlier Unicode 17 assumptions.
    each_split(
        "\u{94d}\u{308}\u{915}".as_bytes(),
        &[
            MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
            MarkdownGraphemeBoundary { byte: 8, utf16: 3 },
        ],
    );
    each_split(
        "👩\u{200d}\u{1f1ae}".as_bytes(),
        &[
            MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
            MarkdownGraphemeBoundary { byte: 7, utf16: 3 },
            MarkdownGraphemeBoundary { byte: 11, utf16: 5 },
        ],
    );
}

#[test]
fn utf8_step_and_source_split_do_not_forge_eof_or_negative_boundary() {
    // A byte split inside the combining scalar leaves its boundary unknown until the
    // original machine decodes enough input. A visitor edge supplies neither EOF nor NoBreak.
    let source = source("a\u{308}b".as_bytes());
    let authority = source.authority();
    let mut job = MarkdownGraphemeJob::new(source).unwrap();
    let observer = job.resource_observer();
    job.split = Some(2);
    let one = MarkdownGraphemeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
    };
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Byte,
            0,
            MarkdownGraphemeQuery::IsBoundary
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Pending
    );
    job.step(&authority, one).unwrap();
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Byte,
            0,
            MarkdownGraphemeQuery::Next
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Pending
    );
    let partial = job.step(&authority, one).unwrap();
    assert_eq!(partial.scalars, 0);
    assert!(!job.is_ready());
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Byte,
            1,
            MarkdownGraphemeQuery::IsBoundary
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Pending
    );
    finish(&mut job, &authority);
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Byte,
            1,
            MarkdownGraphemeQuery::IsBoundary
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Known(None)
    );
    job.close().unwrap();
    released(&observer, true);
}

#[test]
fn same_bytes_generation_and_empty_root_cannot_replace_original_source_identity() {
    // Equal bytes and generations, including an empty source, do not identify the original capture.
    for bytes in [b"".as_slice(), b"same"] {
        let original = source(bytes);
        let foreign = source(bytes);
        assert_eq!(original.generation(), foreign.generation());
        let original_authority = original.authority();
        let mut job = MarkdownGraphemeJob::new(original).unwrap();
        let observer = job.resource_observer();
        finish(&mut job, &original_authority);
        assert!(matches!(
            job.query(
                &foreign.authority(),
                MarkdownGraphemeUnit::Byte,
                0,
                MarkdownGraphemeQuery::IsBoundary
            ),
            Err(MarkdownGraphemeError::SourceMismatch)
        ));
        assert!(!job.is_ready());
        assert!(matches!(
            job.step(&original_authority, TINY),
            Err(MarkdownGraphemeError::Poisoned)
        ));
        drop(job);
        released(&observer, true);
    }
}

#[test]
fn cancelled_and_invalid_budget_or_query_never_restore_old_proof() {
    // Even a previously completed result cannot survive cancellation, a zero quantum,
    // or an invalid query. Returning to valid inputs must not restore old authority.
    for mode in 0..3 {
        let original = source(b"abc");
        let authority = original.authority();
        let mut job = MarkdownGraphemeJob::new(original).unwrap();
        let observer = job.resource_observer();
        finish(&mut job, &authority);
        match mode {
            0 => job.cancel(),
            1 => assert!(matches!(
                job.step(
                    &authority,
                    MarkdownGraphemeBudget {
                        bytes: 0,
                        scalars: 1,
                        records: 1
                    }
                ),
                Err(MarkdownGraphemeError::InvalidBudget)
            )),
            _ => assert!(matches!(
                job.query(
                    &authority,
                    MarkdownGraphemeUnit::Utf16,
                    4,
                    MarkdownGraphemeQuery::Next
                ),
                Err(MarkdownGraphemeError::InvalidRange)
            )),
        }
        assert!(!job.is_ready());
        assert!(matches!(
            job.query(
                &authority,
                MarkdownGraphemeUnit::Byte,
                0,
                MarkdownGraphemeQuery::Next
            ),
            Err(MarkdownGraphemeError::Poisoned)
        ));
        drop(job);
        released(&observer, true);
    }
}

#[test]
fn metadata_failure_rejects_published_cache_hits_and_releases_real_resources() {
    // Preserve the existing write-failure injection: a later metadata failure invalidates
    // even an earlier boundary that was already resident and successfully published.
    let original = source(b"abc");
    let authority = original.authority();
    let mut job = MarkdownGraphemeJob::new_paged(original).unwrap();
    let observer = job.resource_observer();
    job.index.fail_record = Some(1);
    assert!(job.step(&authority, TINY).is_ok());
    assert!(matches!(
        job.step(&authority, TINY),
        Err(MarkdownGraphemeError::Storage(_))
    ));
    assert!(matches!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Byte,
            0,
            MarkdownGraphemeQuery::IsBoundary
        ),
        Err(MarkdownGraphemeError::Poisoned)
    ));
    drop(job);
    released(&observer, true);
}

#[test]
fn real_readonly_fd_error_is_permanent_without_a_shadow_source_file() {
    let (directory, document) = fixture(&b"a".repeat(2048));
    let original = MarkdownLayoutTextSource::capture(&document);
    let authority = original.authority();
    let mut job = MarkdownGraphemeJob::new_paged(original).unwrap();
    // The fresh test index uses a real read-only FD. Its first page fits the original
    // buffer; crossing the page makes the OS actually reject write_all_at.
    job.index =
        store::BoundaryIndex::with_file(File::open(directory.path().join("graphemes.md")).unwrap())
            .unwrap();
    let observer = job.resource_observer();
    let mut failure = None;
    for _ in 0..32 {
        match job.step(&authority, MarkdownGraphemeBudget::default()) {
            Ok(_) => {}
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    assert!(
        matches!(failure, Some(MarkdownGraphemeError::Storage(error)) if error.raw_os_error() == Some(libc::EBADF))
    );
    assert!(matches!(
        job.step(&authority, TINY),
        Err(MarkdownGraphemeError::Poisoned)
    ));
    // Cleanup still returns the real FD and buffer, while retaining the unsuccessful
    // write/cleanup fact instead of reporting a successful resource release.
    drop(document);
    drop(job);
    let released = observer.snapshot();
    assert!(
        !released.metadata_file_open
            && released.metadata_buffer_bytes == 0
            && released.metadata_map_bytes == 0
    );
    assert!(released.cleanup_failed && released.source_payload_released);
    assert_eq!(
        released.metadata_peak_buffer_bytes,
        MARKDOWN_GRAPHEME_INDEX_BYTES
    );
}

#[test]
fn snapshot_survives_edit_undo_and_document_drop_without_retaining_the_document() {
    let (_directory, mut document) = fixture("a\u{308}b".as_bytes());
    let original = MarkdownLayoutTextSource::capture(&document);
    let authority = original.authority();
    let generation = original.generation();
    let mut job = MarkdownGraphemeJob::new(original).unwrap();
    let observer = job.resource_observer();
    document
        .replace_utf16_range(0..1, "中文😀", MarkdownTextSelection::caret(0))
        .unwrap();
    assert_ne!(document.generation(), generation);
    document.undo().unwrap();
    assert_ne!(document.generation(), generation);
    let current = MarkdownLayoutTextSource::capture(&document);
    assert_ne!(current.generation(), generation);
    // The document's Weak is actually dead while the old Snapshot remains readable.
    // The layout job owns no document, undo history, or publication authority.
    let document = Rc::new(document);
    let weak_document = Rc::downgrade(&document);
    drop(document);
    assert!(weak_document.upgrade().is_none());
    drop(current);
    finish(&mut job, &authority);
    compare(
        &mut job,
        &authority,
        &[
            MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
            MarkdownGraphemeBoundary { byte: 3, utf16: 2 },
            MarkdownGraphemeBoundary { byte: 4, utf16: 3 },
        ],
    );
    job.close().unwrap();
    released(&observer, true);
}

#[test]
fn source_clone_extends_only_immutable_payload_and_index_pages_remain_single() {
    // This explicitly finite array is an oracle input, not a production document copy.
    let bytes = b"a".repeat(20000);
    let original = source(&bytes);
    let authority = original.authority();
    let mut job = MarkdownGraphemeJob::new_paged(original.clone()).unwrap();
    let observer = job.resource_observer();
    finish(&mut job, &authority);
    let profile = job.storage_profile();
    assert_eq!(profile.boundary_count, 20001);
    assert!(profile.source_borrow_max_bytes <= 4096);
    assert_eq!(profile.unicode_version, (18, 0, 0));
    for byte in [20000, 0, 1024, 17000, 8193, 16384] {
        assert_eq!(
            job.query(
                &authority,
                MarkdownGraphemeUnit::Byte,
                byte,
                MarkdownGraphemeQuery::IsBoundary
            )
            .unwrap(),
            MarkdownGraphemeAnswer::Known(Some(MarkdownGraphemeBoundary { byte, utf16: byte }))
        );
        assert_eq!(observer.snapshot().metadata_map_bytes, 0);
        assert_eq!(
            observer.snapshot().metadata_buffer_bytes,
            MARKDOWN_GRAPHEME_INDEX_BYTES
        );
    }
    assert_eq!(job.storage_profile().source_bytes_read, 20000);
    // Closing the index does not destroy the immutable source held by the external clone.
    job.close().unwrap();
    released(&observer, false);
    drop(original);
    released(&observer, true);
}

#[test]
fn huge_single_cluster_stays_pending_until_true_eof_with_bounded_source_borrows() {
    // Stream a long combining sequence into an isolated file. The source is one EGC,
    // so repeated bounded scans cannot certify its successor before actual EOF.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("giant-egc.md");
    {
        use std::io::Write;
        let mut file = std::io::BufWriter::with_capacity(4096, File::create(&path).unwrap());
        file.write_all(b"a").unwrap();
        for _ in 0..10000 {
            file.write_all("\u{308}".as_bytes()).unwrap();
        }
        file.flush().unwrap();
    }
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    let original = MarkdownLayoutTextSource::capture(&document);
    let authority = original.authority();
    let mut job = MarkdownGraphemeJob::new(original).unwrap();
    drop(document);
    for _ in 0..3 {
        assert_eq!(
            job.step(&authority, MarkdownGraphemeBudget::default())
                .unwrap()
                .status,
            MarkdownGraphemeStatus::Pending
        );
        assert_eq!(
            job.query(
                &authority,
                MarkdownGraphemeUnit::Byte,
                0,
                MarkdownGraphemeQuery::Next
            )
            .unwrap(),
            MarkdownGraphemeAnswer::Pending
        );
    }
    finish(&mut job, &authority);
    assert_eq!(job.storage_profile().boundary_count, 2);
    assert!(job.storage_profile().source_borrow_max_bytes <= MARKDOWN_GRAPHEME_SOURCE_BYTES);
}

#[test]
#[ignore = "需要ROOT中官方Unicode18 GraphemeBreakTest与显式OMNI_GCB18_TEST_FILE"]
fn official_853_every_byte_split_and_global_byte_utf16_queries() {
    // The ignored oracle requires the explicitly supplied official file and exact digest.
    // Reading or compiling this test does not run it or establish conformance acceptance.
    let path = std::env::var_os("OMNI_GCB18_TEST_FILE").expect("需要官方ROOT fixture路径");
    let mut hash = Sha256::new();
    let mut input = File::open(&path).unwrap();
    let mut chunk = [0; 8192];
    loop {
        let read = input.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        hash.update(&chunk[..read]);
    }
    assert_eq!(
        format!("{:x}", hash.finalize()),
        "b0cf047ee94485bbdc846de2b902f5f8a815f6b674f9d04223cddadd91c9df31"
    );
    let mut cases = 0;
    let mut jobs = 0;
    let mut queries = 0;
    // Parse each finite official case's own divide markers as the independent oracle.
    // Text encoding only provides coordinates, never inferred grapheme decisions.
    for (line_number, line) in BufReader::new(File::open(path).unwrap())
        .lines()
        .enumerate()
    {
        let line = line.unwrap();
        let text = line.split('#').next().unwrap().trim();
        if text.is_empty() {
            continue;
        }
        let mut bytes = Vec::new();
        let mut expected = Vec::new();
        let mut utf16 = 0;
        for token in text.split_whitespace() {
            match token {
                "÷" => expected.push(MarkdownGraphemeBoundary {
                    byte: bytes.len(),
                    utf16,
                }),
                "×" => {}
                scalar => {
                    let scalar = char::from_u32(u32::from_str_radix(scalar, 16).unwrap()).unwrap();
                    let mut encoded = [0; 4];
                    bytes.extend_from_slice(scalar.encode_utf8(&mut encoded).as_bytes());
                    utf16 += scalar.len_utf16();
                }
            }
        }
        assert!(bytes.len() <= 4096, "完整oracle仅限官方有限case");
        cases += 1;
        // Test both origin and nonzero global coordinates, retaining the real leading
        // newline context before trying every actual byte split of the input.
        for prefix in [false, true] {
            let mut actual_bytes = if prefix {
                "🧭\n".as_bytes().to_vec()
            } else {
                Vec::new()
            };
            let offset = actual_bytes.len();
            actual_bytes.extend_from_slice(&bytes);
            let mut oracle = if prefix {
                vec![
                    MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
                    MarkdownGraphemeBoundary { byte: 4, utf16: 2 },
                ]
            } else {
                Vec::new()
            };
            oracle.extend(expected.iter().map(|b| MarkdownGraphemeBoundary {
                byte: b.byte + offset,
                utf16: b.utf16 + if prefix { 3 } else { 0 },
            }));
            let original = source(&actual_bytes);
            let authority = original.authority();
            for split in 0..=actual_bytes.len() {
                let mut job = MarkdownGraphemeJob::new_paged(original.clone()).unwrap();
                job.split = Some(split);
                finish(&mut job, &authority);
                assert_eq!(
                    job.storage_profile().boundary_count,
                    oracle.len(),
                    "line={} split={split}",
                    line_number + 1
                );
                queries += compare(&mut job, &authority, &oracle);
                let observer = job.resource_observer();
                job.close().unwrap();
                released(&observer, false);
                jobs += 1;
            }
        }
    }
    assert_eq!(cases, 853);
    assert_eq!(jobs, 16591);
    assert_eq!(queries, 973437);
    println!("official_core_gcb18 cases={cases} jobs={jobs} queries={queries} mismatches=0");
}

#[test]
fn source_reclaim_failure_overrides_quota_stop_and_revokes_completed_index() {
    // A real original-window reclaim failure must override a local quota stop.
    let (_directory, document) = fixture(b"abcdef");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mut job = MarkdownGraphemeJob::new_paged(source.clone()).unwrap();
    poison_original_read_windows(&document);
    let result = job.step(
        &authority,
        MarkdownGraphemeBudget {
            bytes: 6,
            scalars: 1,
            records: 1,
        },
    );
    assert!(matches!(
        result,
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(!job.is_ready());
    assert!(matches!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Byte,
            0,
            MarkdownGraphemeQuery::IsBoundary
        ),
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));

    // Repeat after complete Ready to prove that cached index results cannot conceal a
    // later failure of the same original source payload.
    let (_directory, other_document) = fixture(b"xyz");
    let source = MarkdownLayoutTextSource::capture(&other_document);
    let authority = source.authority();
    let mut completed = MarkdownGraphemeJob::new(source).unwrap();
    finish(&mut completed, &authority);
    assert!(completed.is_ready());
    poison_original_read_windows(&other_document);
    let _ = other_document.utf16_code_unit_at(0).unwrap_err();
    assert!(!completed.is_ready());
    assert!(matches!(
        completed.query(
            &authority,
            MarkdownGraphemeUnit::Byte,
            0,
            MarkdownGraphemeQuery::IsBoundary
        ),
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
}

#[cfg(feature = "paged-shaping-frontier")]
#[path = "dense_tests.rs"]
mod dense_tests;
