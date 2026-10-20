//! 仅有限合成oracle允许完整String/Vec；产品状态始终只保留固定cluster摘要和分页metadata。
use super::*;
use crate::MarkdownDocumentBuffer;

#[cfg(feature = "paged-line-compose")]
#[path = "fold_tests.rs"]
mod fold_tests;

/// 先真实借入一页，再在公开测试驱逐点制造I/O失败；归还pin本身不代表msync发生。
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
    ); // 只有真实无pin页回收已失败，后继算法才应观察同一个永久源错误。
}

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
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
    // 原规则/配额/故障测试固定保留逐scalar Machine；新摘要另以同一原路径作差分。
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
    // 最后closer的opening早于96层；丢弃底部32层的实现会把它错误继承为Hebrew。
    let reset = tagged("a(α\r\nא)", MarkdownScriptBudget::default());
    assert_eq!(reset.last().unwrap().1, *b"Hebr");
    assert!(!reset.iter().any(|(part, _)| part.starts_with('\n'))); // CRLF仍完整，不在Script阶段拆开原GCB。
    let quotes = tagged("a'α'א", MarkdownScriptBudget::default());
    assert_eq!(quotes[1], ("α'".into(), *b"Grek")); // 引号角色依语言，不能仅按码点伪造一对括号。
}

#[test]
fn pending_prefix_and_every_byte_split_preserve_global_utf16_coverage() {
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
fn authority_cancel_source_failure_and_real_release_are_not_replaced_by_cached_runs() {
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
    assert!(mapping.unmap_succeeded() && mapping.failed()); // 真实unmap成功不抹掉此前已注入的源回收失败。
    drop(directory);
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
    // 尚未推进任何记录时没有metadata FD，原worker仍独占活Source并须通过join实际归还。
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
    assert!(mapping.unmap_succeeded() && !mapping.failed()); // 只有取消、没有I/O失败的线程仍须实际干净释放映射。
}

#[test]
fn unicode18_generated_scx_sets_and_bracket_cluster_assumptions_match_exact_tables() {
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
    finish(&mut ready, &authority, MarkdownScriptBudget::default());
    ready.cancellation.cancel();
    for _ in 0..2 {
        assert!(matches!(
            ready.run_at(&authority, MarkdownScriptUnit::Byte, 0),
            Err(MarkdownScriptError::Cancelled)
        ));
    }
    let (directory, _document, authority, mut broken) = make_job(&"aא".repeat(300));
    let path = directory.path().join("readonly-index");
    std::fs::write(&path, []).unwrap();
    broken.store =
        store::Store::with_runs_file(std::fs::File::open(path).unwrap(), &broken.cancellation)
            .unwrap();
    let observer = broken.resource_observer();
    // 真正跨过256条run才写第一页；短private seal在新协议下不需要File消费者。
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
