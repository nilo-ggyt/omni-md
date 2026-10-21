//! Compare fine quanta with an independent finite UBA oracle; release assertions observe actual FD/pages and original backing destruction.
use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource};
use unicode_bidi::utf16::BidiInfo;

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
    source: MarkdownLayoutTextSource,
}
impl Fixture {
    fn new(text: &str) -> Self {
        // A private synthetic file enters the actual document/immutable Source graph; no user vault participates in these writes.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prepare.md");
        std::fs::write(&path, text).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        Self {
            _directory: directory,
            document,
            source,
        }
    }
    fn job(
        &self,
        base: BidiBaseDirection,
        cancel: &MarkdownBidiCancellation,
    ) -> MarkdownBidiPrepareJob {
        MarkdownBidiPrepareJob::new_layout_source(
            self.source.clone(),
            &self.source.authority(),
            base,
            BidiMemoryBudget::new(65536).unwrap(),
            cancel,
        )
        .unwrap()
    }
}

fn assert_quantum(step: MarkdownBidiPrepareStep, budget: MarkdownBidiPrepareBudget) {
    // Preserve the existing logical-operation accounting checks; these are not syscall counts, UI latency, or whole-product RSS.
    assert!(step.bytes <= budget.bytes);
    assert!(step.scalars <= budget.scalars);
    assert!(step.metadata_ops <= budget.metadata_ops);
    assert_eq!(
        step.metadata_ops,
        step.record_reads + step.record_writes + step.resource_ops
    );
    assert!(step.source_borrow_max_bytes <= budget.bytes.min(4096));
}

fn complete(
    job: &mut MarkdownBidiPrepareJob,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownBidiPrepareBudget,
) -> (MarkdownBidiParagraph, usize) {
    // Finite synthetic input has an explicit hang bound; reaching it fails instead of treating unfinished work as Ready.
    for polls in 1..2_000_000 {
        let progress = job.step(authority, budget).unwrap();
        assert_quantum(progress, budget);
        if progress.status == MarkdownBidiPrepareStatus::Ready {
            return (job.take_ready(authority).unwrap().unwrap(), polls);
        }
        assert!(job.take_ready(authority).unwrap().is_none());
    }
    panic!("有限UBA夹具没有在实际预算推进中结束");
}

fn assert_oracle(text: &str, base: BidiBaseDirection, actual: &mut MarkdownBidiParagraph) {
    assert!(text.chars().count() <= 4096); // Only this explicitly finite test oracle may construct complete arrays.
    let units: Vec<u16> = text.encode_utf16().collect();
    let oracle = BidiInfo::new_with_data_source(
        &UnicodeBidiData,
        &units,
        base.level()
            .map(|level| unicode_bidi::Level::new(level).unwrap()),
    );
    for (position, expected) in oracle.levels.iter().enumerate() {
        assert_eq!(actual.level_at_utf16(position).unwrap(), expected.number());
    }
    assert_eq!(actual.actual_paragraph_count(), oracle.paragraphs.len());
    let mut offsets = Vec::new();
    // Compare actual scalar visual order and each L1-adjusted level; UTF16 continuation records must not become independent visible characters.
    let mut offset = 0;
    for scalar in text.chars() {
        offsets.push(offset);
        offset += scalar.len_utf16();
    }
    offsets.push(offset);
    for paragraph in &oracle.paragraphs {
        let first = offsets.binary_search(&paragraph.range.start).unwrap();
        let last = offsets.binary_search(&paragraph.range.end).unwrap();
        let levels = oracle.reordered_levels_per_char(paragraph, paragraph.range.clone());
        let expected: Vec<_> = BidiInfo::reorder_visual(&levels[first..last])
            .into_iter()
            .map(|index| offsets[first + index])
            .collect();
        let mut observed = Vec::new();
        actual
            .visit_visual_runs(paragraph.range.clone(), |run| {
                let from = offsets.binary_search(&run.source_utf16_start).unwrap();
                let to = offsets.binary_search(&run.source_utf16_end).unwrap();
                if run.level & 1 == 0 {
                    observed.extend_from_slice(&offsets[from..to]);
                } else {
                    observed.extend(offsets[from..to].iter().rev().copied());
                }
                for &position in &offsets[from..to] {
                    assert_eq!(
                        run.level,
                        levels[offsets.binary_search(&position).unwrap()].number()
                    );
                }
            })
            .unwrap();
        assert_eq!(observed, expected, "原标量的L1/L2顺序必须保持");
    }
}

#[test]
fn one_unit_quanta_preserve_nested_backfill_and_independent_uba_oracle() {
    // Resume the same UBA rules under several tiny quotas, including nested isolate/bracket backfill, then compare complete actual output.
    for text in [
        "",
        "中😀 A123\u{0301}\n",
        "אב $\u{00ad}$$12,34 +\u{0301}5 (a [ב]) \t\n中😀",
        "a\u{2068}12\u{2067}אב\u{2066}中😀\u{2069}ج\u{2069}\u{2069}z",
        "א\u{202b}12 [a\u{202a}😀] \u{202c}ב\u{202c}\r\n尾",
    ] {
        for base in [
            BidiBaseDirection::Auto,
            BidiBaseDirection::LeftToRight,
            BidiBaseDirection::RightToLeft,
        ] {
            let fixture = Fixture::new(text);
            for budget in [
                MarkdownBidiPrepareBudget {
                    bytes: 1,
                    scalars: 1,
                    metadata_ops: 1,
                },
                MarkdownBidiPrepareBudget {
                    bytes: 7,
                    scalars: 3,
                    metadata_ops: 5,
                },
                MarkdownBidiPrepareBudget::default(),
            ] {
                let mut job = fixture.job(base, &MarkdownBidiCancellation::new());
                let receipt = job.resource_observer();
                let (mut actual, _) = complete(&mut job, &fixture.source.authority(), budget);
                assert_oracle(text, base, &mut actual);
                assert!(matches!(
                    job.take_ready(&fixture.source.authority()),
                    Err(BidiError::PoisonedMetadata)
                ));
                drop(actual);
                drop(job);
                assert!(receipt.is_released());
            }
        }
    }
}

#[test]
fn source_quota_one_does_not_read_a_window_or_end_an_incomplete_scalar() {
    let mut text = "中😀\u{0301}".repeat(64);
    // A complete root may prove level zero without reads; its actual LF position still requires the same-Source endpoint cursor.
    // A one-byte quantum cannot finish half a CJK/emoji scalar or borrow a complete leaf and report only one byte.
    text.push('\n');
    let fixture = Fixture::new(&text);
    let mut job = fixture.job(BidiBaseDirection::Auto, &MarkdownBidiCancellation::new());
    let authority = fixture.source.authority();
    let budget = MarkdownBidiPrepareBudget {
        bytes: 1,
        scalars: 1,
        metadata_ops: 1,
    };
    let first = job.step(&authority, budget).unwrap();
    #[cfg(feature = "paged-shaping-frontier")]
    let (mut actual, polls) = {
        assert_eq!((first.bytes, first.scalars, first.metadata_ops), (0, 0, 0));
        assert_eq!(first.source_borrow_max_bytes, 0);
        assert_eq!(first.status, MarkdownBidiPrepareStatus::Ready);
        let mut actual = job.take_ready(&authority).unwrap().unwrap();
        let mut incomplete_scalar = false;
        let mut polls = 0;
        loop {
            let step = actual
                .prepare_uniform_paragraph_range(&authority, 0, budget)
                .unwrap();
            assert!(step.bytes <= 1 && step.scalars <= 1 && step.metadata <= 1);
            assert!(step.maximum_borrow <= 1);
            incomplete_scalar |= step.bytes == 1 && step.scalars == 0;
            polls += 1;
            if step.ready {
                break;
            }
            assert!(polls < 2_000_000); // The original finite Source must actually finish; perpetual Pending is not success.
        }
        assert!(incomplete_scalar);
        (actual, polls)
    };
    #[cfg(not(feature = "paged-shaping-frontier"))]
    let (mut actual, polls) = {
        assert_eq!((first.bytes, first.scalars, first.metadata_ops), (1, 0, 0));
        assert_eq!(first.source_borrow_max_bytes, 1);
        assert_eq!(first.status, MarkdownBidiPrepareStatus::Pending);
        assert!(job.take_ready(&authority).unwrap().is_none());
        complete(&mut job, &authority, budget) // Without the summary fast path, retain the original Reader's actual incomplete-UTF8 contract.
    };
    assert!(polls >= text.len() - 1);
    assert_eq!(
        actual.paragraph_range_at_utf16(0).unwrap(),
        Some(0..text.encode_utf16().count())
    );
    assert_oracle(&text, BidiBaseDirection::Auto, &mut actual);
    let resources = job.resource_observer().snapshot();
    assert!(!resources.file_created && resources.cache_bytes == 0);
    assert_eq!(actual.storage_profile().source_borrow_max_bytes, 1);
}

#[test]
fn dropping_pending_future_after_real_page_allocation_releases_original_owners() {
    // Reach the actual first allocated page through public progress/resource facts, then Drop the still-pending future and its real owners.
    let fixture = Fixture::new("אב\u{2068}12😀\u{2069}Tail");
    let source_observer = fixture.source.resource_observer();
    let mapping = fixture.document.original_backing_observer().unwrap();
    let authority = fixture.source.authority();
    let mut job = fixture.job(BidiBaseDirection::Auto, &MarkdownBidiCancellation::new());
    let receipt = job.resource_observer();
    drop(fixture);
    let budget = MarkdownBidiPrepareBudget {
        bytes: 1,
        scalars: 1,
        metadata_ops: 1,
    };
    for _ in 0..100 {
        let step = job.step(&authority, budget).unwrap();
        assert_quantum(step, budget);
        assert_eq!(step.status, MarkdownBidiPrepareStatus::Pending);
        let resource = receipt.snapshot();
        if resource.cache_bytes == store::PAGE_BYTES {
            assert!(resource.file_open && !resource.close_succeeded);
            assert!(!source_observer.payload_released());
            drop(job); // Destroy the actual future-local Store; unfinished work must not retain its FD, pages, or Source.
            let after = receipt.snapshot();
            assert!(after.close_succeeded && !after.file_open && after.cache_bytes == 0);
            assert!(receipt.is_released());
            assert!(source_observer.payload_released());
            assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
            return;
        }
    }
    panic!("测试未到达真实首个缓存页，而非按时间猜构造进度");
}

#[test]
fn cancellation_before_during_and_after_preparation_is_permanent_and_resource_complete() {
    // Cover cancellation before allocation, with a real pending page, and after actual Ready; none may retain a publishable candidate or owned metadata.
    for phase in 0..3 {
        let fixture = Fixture::new("אב\u{2068}12😀\u{2069}Tail");
        let cancel = MarkdownBidiCancellation::new();
        let source_observer = fixture.source.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        let authority = fixture.source.authority();
        if phase == 0 {
            cancel.cancel();
        }
        let (result, receipt) = MarkdownBidiPrepareJob::new_layout_source_observed(
            fixture.source.clone(),
            &authority,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        );
        if phase == 0 {
            assert!(matches!(result, Err(BidiError::Cancelled)));
            assert!(!receipt.snapshot().file_created);
        } else {
            let mut job = result.unwrap();
            if phase == 1 {
                let budget = MarkdownBidiPrepareBudget {
                    bytes: 1,
                    scalars: 1,
                    metadata_ops: 1,
                };
                while receipt.snapshot().cache_bytes == 0 {
                    assert_eq!(
                        job.step(&authority, budget).unwrap().status,
                        MarkdownBidiPrepareStatus::Pending
                    );
                }
            } else {
                while job
                    .step(&authority, MarkdownBidiPrepareBudget::default())
                    .unwrap()
                    .status
                    != MarkdownBidiPrepareStatus::Ready
                {}
            }
            cancel.cancel();
            assert!(matches!(
                job.step(&authority, MarkdownBidiPrepareBudget::default()),
                Err(BidiError::Cancelled)
            ));
            assert!(matches!(
                job.take_ready(&authority),
                Err(BidiError::Cancelled)
            ));
            assert!(receipt.is_released()); // The failure entry already dropped the original future/Ready owner, without waiting for the outer Job to die.
            drop(job);
        }
        assert!(receipt.is_released());
        drop(fixture);
        assert!(source_observer.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn original_source_error_dominates_foreign_ticket_cancel_and_ready_hits() {
    // Both pending and cached-Ready paths must observe the original actual Source errno before a foreign authority or later cancellation.
    for ready in [false, true] {
        let fixture = Fixture::new("אב😀Tail");
        let cancel = MarkdownBidiCancellation::new();
        let mut job = fixture.job(BidiBaseDirection::Auto, &cancel);
        let receipt = job.resource_observer();
        if ready {
            while job
                .step(
                    &fixture.source.authority(),
                    MarkdownBidiPrepareBudget::default(),
                )
                .unwrap()
                .status
                != MarkdownBidiPrepareStatus::Ready
            {}
        } else {
            job.step(
                &fixture.source.authority(),
                MarkdownBidiPrepareBudget {
                    bytes: 1,
                    scalars: 1,
                    metadata_ops: 1,
                },
            )
            .unwrap();
        }
        // Perform an actual source read before failing reclamation; an untriggered test flag cannot stand in for an observed errno.
        fixture.source.visit_byte_range(0..1, |_| Ok(())).unwrap();
        fixture.document.fail_next_original_reclaim_for_test();
        assert_eq!(
            fixture.document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        );
        cancel.cancel();
        let foreign = MarkdownLayoutTextSource::capture(&fixture.document).authority();
        assert!(matches!(
            job.step(&foreign, MarkdownBidiPrepareBudget::default()),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert!(matches!(
            job.take_ready(&foreign),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert!(receipt.is_released());
    }
}

#[test]
fn partial_actual_metadata_write_failure_never_becomes_ready_or_later_cancelled() {
    // Retain the existing real-prefix-write failure seam and verify that its stable Storage cause survives later cancellation and repeated calls.
    let fixture = Fixture::new("אב (12😀) Tail");
    let cancel = MarkdownBidiCancellation::new();
    let mut job = fixture.job(BidiBaseDirection::Auto, &cancel);
    let receipt = job.resource_observer();
    job.gate.fail_next_flush();
    let budget = MarkdownBidiPrepareBudget {
        bytes: 3,
        scalars: 2,
        metadata_ops: 1,
    };
    for _ in 0..100_000 {
        match job.step(&fixture.source.authority(), budget) {
            Ok(progress) => {
                assert_quantum(progress, budget);
                assert_eq!(progress.status, MarkdownBidiPrepareStatus::Pending);
            }
            Err(BidiError::Storage(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::Other);
                assert!(job.last_step().resource_ops > 0);
                cancel.cancel();
                assert!(
                    matches!(job.take_ready(&fixture.source.authority()), Err(BidiError::Storage(error)) if error.kind() == std::io::ErrorKind::Other)
                );
                assert!(matches!(
                    job.step(&fixture.source.authority(), budget),
                    Err(BidiError::Storage(_))
                ));
                let actual = receipt.snapshot();
                assert!(
                    actual.file_created
                        && actual.close_succeeded
                        && !actual.file_open
                        && actual.cache_bytes == 0
                );
                assert!(receipt.is_released());
                return;
            }
            Err(error) => panic!("真实部分写的首因丢失: {error:?}"),
        }
    }
    panic!("没有执行真实部分页写故障");
}

#[test]
fn foreign_or_bad_budget_revokes_pending_and_empty_ready_still_checks_source() {
    // Wrong authority and unusable quota are permanent failures; an actually empty completed input still retains original cancellation checks.
    let fixture = Fixture::new("אב😀");
    let mut job = fixture.job(BidiBaseDirection::Auto, &MarkdownBidiCancellation::new());
    let foreign = MarkdownLayoutTextSource::capture(&fixture.document).authority();
    assert!(matches!(
        job.step(&foreign, MarkdownBidiPrepareBudget::default()),
        Err(BidiError::SourceMismatch)
    ));
    assert!(matches!(
        job.step(
            &fixture.source.authority(),
            MarkdownBidiPrepareBudget::default()
        ),
        Err(BidiError::SourceMismatch)
    ));
    let mut invalid = fixture.job(BidiBaseDirection::Auto, &MarkdownBidiCancellation::new());
    assert!(matches!(
        invalid.step(
            &fixture.source.authority(),
            MarkdownBidiPrepareBudget {
                bytes: 0,
                scalars: 1,
                metadata_ops: 1
            }
        ),
        Err(BidiError::InvalidBudget)
    ));
    assert!(matches!(
        invalid.take_ready(&fixture.source.authority()),
        Err(BidiError::InvalidBudget)
    ));
    let empty = Fixture::new("");
    let cancel = MarkdownBidiCancellation::new();
    let mut job = empty.job(BidiBaseDirection::Auto, &cancel);
    let step = job
        .step(
            &empty.source.authority(),
            MarkdownBidiPrepareBudget {
                bytes: 1,
                scalars: 1,
                metadata_ops: 1,
            },
        )
        .unwrap();
    assert_eq!(step.status, MarkdownBidiPrepareStatus::Ready);
    assert_eq!((step.bytes, step.scalars, step.metadata_ops), (0, 0, 0));
    assert!(!job.resource_observer().snapshot().file_created);
    cancel.cancel();
    assert!(matches!(
        job.take_ready(&empty.source.authority()),
        Err(BidiError::Cancelled)
    ));
}

#[test]
fn original_source_moved_to_worker_and_pending_future_dropped_before_join_releases_mapping() {
    // Construct the non-Send Job inside its actual worker, return only finite release observations, and join before checking final Source unmap.
    let fixture = Fixture::new("אב\u{2068}12😀\u{2069}Tail");
    let mapping = fixture.document.original_backing_observer().unwrap();
    let source_observer = fixture.source.resource_observer();
    let source = fixture.source.clone();
    drop(fixture);
    let worker = std::thread::spawn(move || {
        let authority = source.authority();
        let mut job = MarkdownBidiPrepareJob::new_layout_source(
            source,
            &authority,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &MarkdownBidiCancellation::new(),
        )
        .unwrap();
        let receipt = job.resource_observer();
        let budget = MarkdownBidiPrepareBudget {
            bytes: 1,
            scalars: 1,
            metadata_ops: 1,
        };
        while receipt.snapshot().cache_bytes == 0 {
            assert_eq!(
                job.step(&authority, budget).unwrap().status,
                MarkdownBidiPrepareStatus::Pending
            );
        }
        drop(job); // Destroy the future in the worker that created it; no non-Send Job crosses threads.
        receipt
    });
    let receipt = worker.join().unwrap();
    assert!(receipt.is_released() && receipt.snapshot().close_succeeded);
    assert!(source_observer.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[cfg(feature = "s2-table-window-prototype")]
#[test]
fn draft_content_revocation_stops_metadata_only_quantum_before_cancellation() {
    // Prepare an actual certified cell draft, reach metadata-only work, then change its contentSerial through the public edit path.
    use crate::{
        MarkdownTextSelection,
        markdown_document_buffer::{MarkdownTableWindowStep, MarkdownTableWindowTarget},
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("草稿预算.md");
    std::fs::write(&path, "| H | C |\n| --- | --- |\n| a | b |\n").unwrap();
    let mut document = MarkdownDocumentBuffer::open(&path, 65536).unwrap();
    let mut request = document
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(0),
            0..document.utf16_len(),
            1,
            0..0,
        )
        .unwrap();
    let verified = loop {
        match request.advance(&document, false, 4096).unwrap() {
            MarkdownTableWindowStep::Progress { .. } => {}
            MarkdownTableWindowStep::Ready { .. } => {
                break request.take_ready_for(&document).unwrap().unwrap();
            }
            other => panic!("真实有限表格未认证: {other:?}"),
        }
    };
    let draft = document.begin_cell_draft(&verified, 1, 0).unwrap();
    let edit = document.capture_cell_draft_edit(&draft).unwrap();
    document
        .replace_cell_draft(edit, 0..1, "abc אבג", MarkdownTextSelection::caret(7), None)
        .unwrap();
    let source = MarkdownLayoutTextSource::capture_cell_draft(&document, &draft).unwrap();
    let source_observer = source.resource_observer();
    let authority = source.authority();
    let mapping = document.original_backing_observer().unwrap();
    let cancel = MarkdownBidiCancellation::new();
    let mut job = MarkdownBidiPrepareJob::new_layout_source(
        source.clone(),
        &authority,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
        &cancel,
    )
    .unwrap();
    let receipt = job.resource_observer();
    let budget = MarkdownBidiPrepareBudget {
        bytes: 1,
        scalars: 1,
        metadata_ops: 1,
    };
    let mut metadata_only = false;
    for _ in 0..1000 {
        let step = job.step(&authority, budget).unwrap();
        assert_quantum(step, budget);
        assert_eq!(step.status, MarkdownBidiPrepareStatus::Pending);
        if step.bytes == 0 && step.record_reads != 0 {
            metadata_only = true;
            break;
        }
    }
    assert!(metadata_only && receipt.snapshot().file_open); // Actual counters establish metadata work, without guessing the phase from elapsed time.
    let edit = document.capture_cell_draft_edit(&draft).unwrap();
    document
        .replace_cell_draft(edit, 0..1, "z", MarkdownTextSelection::caret(1), None)
        .unwrap();
    cancel.cancel();
    assert!(matches!(
        job.step(&authority, budget),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::PreparedTextInputStale
        ))
    ));
    assert!(matches!(
        job.take_ready(&authority),
        Err(BidiError::Source(
            MarkdownDocumentBufferError::PreparedTextInputStale
        ))
    ));
    assert!(receipt.is_released() && receipt.snapshot().close_succeeded);
    drop(job);
    drop(source);
    drop(document);
    assert!(source_observer.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
#[ignore = "需OMNI_BIDI_PREPARE_PROFILE指向进程启动前预写的实际合成长文件"]
fn actual_file_profile_reports_each_quantum_and_final_resource_release() {
    // This explicitly ignored component profile requires a prewritten isolated fixture; a default run does not execute it or establish product acceptance.
    let path = std::env::var_os("OMNI_BIDI_PREPARE_PROFILE").expect("必须指定实际预写文件");
    let path = std::path::Path::new(&path);
    let length = usize::try_from(std::fs::metadata(path).unwrap().len()).unwrap();
    // Use the original production constructor without reading or generating another complete String/Vec input.
    let document =
        MarkdownDocumentBuffer::open(path, u64::try_from(length.max(1)).unwrap()).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let source_observer = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let cancel = MarkdownBidiCancellation::new();
    let mut job = MarkdownBidiPrepareJob::new_layout_source(
        source,
        &authority,
        BidiBaseDirection::Auto,
        BidiMemoryBudget::new(65536).unwrap(),
        &cancel,
    )
    .unwrap();
    let receipt = job.resource_observer();
    drop(document); // Only actual Prepare/result owners may now retain Source; observations contain atomic facts only.
    let budget = MarkdownBidiPrepareBudget {
        bytes: 4096,
        scalars: 1024,
        metadata_ops: 1024,
    };
    let started = std::time::Instant::now();
    let mut polls = 0usize;
    let mut bytes = 0usize;
    let mut scalars = 0usize;
    let mut records = 0usize;
    let mut maximum = std::time::Duration::ZERO;
    let mut maximum_source = 0usize;
    loop {
        let begin = std::time::Instant::now();
        let step = job.step(&authority, budget).unwrap();
        maximum = maximum.max(begin.elapsed());
        polls += 1;
        assert_quantum(step, budget);
        bytes += step.bytes;
        scalars += step.scalars;
        records += step.metadata_ops;
        maximum_source = maximum_source.max(step.source_borrow_max_bytes);
        if step.status == MarkdownBidiPrepareStatus::Ready {
            break;
        }
    }
    let elapsed = started.elapsed();
    let actual = job.take_ready(&authority).unwrap().unwrap();
    let profile = actual.storage_profile();
    assert!(profile.cache_bytes <= 65536);
    assert!(maximum_source <= 4096);
    let live = receipt.snapshot();
    drop(job);
    drop(actual);
    assert!(receipt.is_released() && !receipt.snapshot().file_open);
    assert!(source_observer.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    eprintln!(
        "bidi_prepare_profile source_bytes={length} utf16={} polls={polls} read_bytes={bytes} decoded_scalars={scalars} metadata_ops={records} borrow_max={maximum_source} elapsed_us={} max_poll_us={} paged={} cache_peak={} metadata_closed={} source_released={} actual_unmap={}",
        profile.utf16_units,
        elapsed.as_micros(),
        maximum.as_micros(),
        live.file_created,
        live.peak_cache_bytes,
        receipt.snapshot().close_succeeded,
        source_observer.payload_released(),
        mapping.unmap_succeeded()
    ); // Actual component work and release observations do not establish whole-product latency or memory compliance.
}
