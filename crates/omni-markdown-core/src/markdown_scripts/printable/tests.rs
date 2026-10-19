//! 原Machine作有限完整oracle；摘要路径的metadata与释放收据仍来自真实文件和映射。

use super::*;
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};
use std::io::Write;

const ONE: MarkdownScriptBudget = MarkdownScriptBudget {
    bytes: 1,
    scalars: 1,
    records: 1,
    stack_ops: 1,
};

fn fixture(text: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("printable-scripts.md");
    std::fs::write(&path, text).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    (directory, document)
}

#[derive(Default)]
struct Work {
    polls: usize,
    bytes: usize,
    scalars: usize,
    records: usize,
    stack_ops: usize,
}

fn finish(
    job: &mut MarkdownScriptJob,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownScriptBudget,
) -> Work {
    let mut work = Work::default();
    for _ in 0..1_000_000 {
        let step = job.step(authority, budget).unwrap();
        assert!(step.bytes <= budget.bytes && step.scalars <= budget.scalars);
        assert!(step.records <= budget.records && step.stack_ops <= budget.stack_ops);
        work.polls += 1;
        work.bytes += step.bytes;
        work.scalars += step.scalars;
        work.records += step.records;
        work.stack_ops += step.stack_ops;
        if step.status == MarkdownScriptStatus::Ready {
            return work;
        }
        assert!(step.bytes + step.scalars + step.records + step.stack_ops > 0);
    }
    panic!("有限脚本夹具没有在真实量子推进中完成");
}

fn runs(
    job: &mut MarkdownScriptJob,
    authority: &MarkdownLayoutTextAuthority,
) -> Vec<MarkdownScriptRun> {
    let mut result = Vec::new();
    for index in 0..job.store.run_count() {
        let run = job.store.run(index).unwrap();
        for (unit, offset) in [
            (MarkdownScriptUnit::Byte, run.start_byte),
            (MarkdownScriptUnit::Utf16, run.start_utf16),
        ] {
            assert_eq!(
                job.run_at(authority, unit, offset).unwrap(),
                MarkdownScriptAnswer::Known(Some(run))
            );
        }
        result.push(run);
    }
    result
}

fn assert_released(observer: &MarkdownScriptResourceObserver) {
    let actual = observer.snapshot();
    assert!(!actual.run_file_open && !actual.stack_file_open);
    assert_eq!(actual.mapped_bytes, 0);
    assert_eq!(actual.buffer_bytes, 0);
    assert!(!actual.cleanup_failed);
    assert!(actual.source_payload_released);
}

fn compare_ascii(text: &str, budget: MarkdownScriptBudget) {
    assert!(text.len() <= 4096 && text.bytes().all(|byte| (b' '..=b'~').contains(&byte)));
    let (_directory, document) = fixture(text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mut actual = MarkdownScriptJob::new(
        source.clone(),
        &authority,
        MarkdownScriptCancellation::new(),
    )
    .unwrap();
    let mut oracle =
        MarkdownScriptJob::new_streaming(source, &authority, MarkdownScriptCancellation::new())
            .unwrap();
    let receipt = actual.resource_observer();
    let oracle_receipt = oracle.resource_observer();
    let before = receipt.snapshot();
    assert!(!before.run_file_open && !before.stack_file_open && before.mapped_bytes == 0);
    assert_eq!(before.buffer_bytes, 0);
    assert_eq!(
        actual
            .run_at(&authority, MarkdownScriptUnit::Byte, 0)
            .unwrap(),
        MarkdownScriptAnswer::Pending
    );
    let work = finish(&mut actual, &authority, budget);
    let oracle_work = finish(&mut oracle, &authority, budget);
    assert_eq!((work.bytes, work.scalars), (0, 0));
    assert_eq!(
        (work.records, work.stack_ops),
        (usize::from(!text.is_empty()), 2)
    );
    assert_eq!(oracle_work.bytes, text.len());
    assert_eq!(oracle_work.scalars, text.len());
    assert_eq!(actual.machine.byte, text.len()); // 语义游标已到真EOF，实际读取量由独立字段保持0。
    assert_eq!(actual.storage_profile().source_bytes_read, 0);
    assert!(actual.machine.ready && actual.storage_profile().printable_summary_used);
    assert!(!oracle.storage_profile().printable_summary_used);
    assert_eq!(actual.storage_profile().bracket_records, 0);
    assert_eq!(actual.storage_profile().source_borrow_max_bytes, 0);
    let actual_runs = runs(&mut actual, &authority);
    let oracle_runs = runs(&mut oracle, &authority);
    assert_eq!(
        actual_runs, oracle_runs,
        "原Machine结果不允许由新摘要改写：{text:?}"
    );
    if let Some(run) = actual_runs.first() {
        assert_eq!(actual_runs.len(), 1);
        assert_eq!((run.start_byte, run.start_utf16), (0, 0));
        assert_eq!((run.end_byte, run.end_utf16), (text.len(), text.len()));
        assert_eq!(
            run.script.iso15924_tag(),
            if text.bytes().any(|byte| byte.is_ascii_alphabetic()) {
                *b"Latn"
            } else {
                *b"Zyyy"
            }
        );
    } else {
        assert!(text.is_empty());
    }
    for position in 0..=text.len() {
        // 每个原坐标都走公开查询；不能只比较内部候选而漏掉EOF或范围定位。
        for unit in [MarkdownScriptUnit::Byte, MarkdownScriptUnit::Utf16] {
            assert_eq!(
                actual.run_at(&authority, unit, position).unwrap(),
                oracle.run_at(&authority, unit, position).unwrap()
            );
        }
    }
    let after = receipt.snapshot();
    // 此摘要只有至多一个真实run，私有封存及逐坐标读取始终落在原单buffer，无需创建File。
    assert!(!after.run_file_open && !after.stack_file_open);
    assert_eq!(after.stack_peak_mapped_bytes, 0);
    assert_eq!(after.run_peak_mapped_bytes, 0);
    assert_eq!(after.stack_peak_buffer_bytes, 0);
    assert_eq!(
        after.run_peak_buffer_bytes,
        if text.is_empty() { 0 } else { 16 * 1024 }
    );
    actual.close().unwrap();
    oracle.close().unwrap();
    assert_released(&receipt);
    assert_released(&oracle_receipt);
}

#[test]
fn every_printable_property_and_complete_machine_oracle_agree_on_summary() {
    let latin = properties::get('a').primary;
    assert_eq!(MarkdownScript(latin).iso15924_tag(), *b"Latn");
    for byte in b' '..=b'~' {
        let p = properties::get(char::from(byte));
        assert!(!p.mark && !p.paragraph);
        if byte.is_ascii_alphabetic() {
            assert_eq!(p.primary, latin);
            assert_eq!(p.scripts, Some(properties::Set::singleton(latin)));
        } else {
            assert_eq!(p.primary, unicode_data::COMMON);
            assert_eq!(p.scripts, None);
        }
        // 每个95字符独立执行旧Machine，防止合并大样本里的Latin上下文掩盖Common事实。
        compare_ascii(&char::from(byte).to_string(), ONE);
    }
    let all: String = (b' '..=b'~').map(char::from).collect();
    compare_ascii(&all, ONE);
    compare_ascii(&all, MarkdownScriptBudget::default());
}

#[test]
fn empty_digits_nested_mismatched_and_pending_pairs_match_original_machine() {
    for text in [
        "",
        "[]",
        "[]a",
        "[]123",
        "[a]",
        "1234567890",
        "  ($1,234.56)  ",
        "([{}])",
        "([{}])a",
        "a([{}])",
        "([)]a",
        "a)]([",
        "([[[a",
        "]]]a",
        "(a)(b)",
        "()[]{}<>!?+-*/\\|#@^~_=:;\"'`",
    ] {
        compare_ascii(text, ONE);
        compare_ascii(text, MarkdownScriptBudget::default());
    }
    for text in [
        format!("{}a{}", "([{".repeat(64), "}])".repeat(64)),
        format!("{}{}a", "([{".repeat(64), ")]}}".repeat(64)),
        format!("{}{}", "(".repeat(96), ")".repeat(96)),
    ] {
        compare_ascii(&text, ONE); // 真实旧栈超过32层；新证明不能靠截断配对记录保持结果。
    }
}

#[test]
fn one_record_and_one_stack_operation_retain_real_pending_phases_without_eager_files() {
    for text in ["", "[]a", "[]"] {
        let (_directory, document) = fixture(text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let mut job =
            MarkdownScriptJob::new(source, &authority, MarkdownScriptCancellation::new()).unwrap();
        let receipt = job.resource_observer();
        let first = job.step(&authority, ONE).unwrap();
        assert_eq!(first.status, MarkdownScriptStatus::Pending);
        assert_eq!((first.bytes, first.scalars), (0, 0));
        assert_eq!(
            (first.records, first.stack_ops),
            (usize::from(!text.is_empty()), 1)
        );
        assert_eq!(job.machine.byte, 0);
        assert!(!job.machine.ready);
        let middle = receipt.snapshot();
        assert!(!middle.run_file_open && !middle.stack_file_open);
        assert_eq!(
            middle.buffer_bytes,
            if text.is_empty() { 0 } else { 16 * 1024 }
        );
        assert_eq!(middle.stack_peak_mapped_bytes, 0);
        assert_eq!(middle.stack_peak_buffer_bytes, 0);
        assert_eq!(job.store.run_count(), usize::from(!text.is_empty()));
        let second = job.step(&authority, ONE).unwrap();
        assert_eq!(second.status, MarkdownScriptStatus::Ready);
        assert_eq!(
            (
                second.bytes,
                second.scalars,
                second.records,
                second.stack_ops
            ),
            (0, 0, 0, 1)
        );
        assert_eq!(receipt.snapshot().mapped_bytes, 0);
        // 私有seal保留非空run的已付费buffer；只有close才归还这份实际分配。
        assert_eq!(
            receipt.snapshot().buffer_bytes,
            if text.is_empty() {
                0
            } else {
                MARKDOWN_SCRIPT_INDEX_BYTES / 2
            }
        );
        let cached = job.step(&authority, ONE).unwrap();
        assert_eq!(cached.status, MarkdownScriptStatus::Ready);
        assert_eq!(
            (
                cached.bytes,
                cached.scalars,
                cached.records,
                cached.stack_ops
            ),
            (0, 0, 0, 0)
        );
        drop(document);
        job.close().unwrap();
        assert_released(&receipt);
    }
}

#[test]
fn large_single_paragraph_writes_one_real_run_without_source_or_bracket_scan() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large-scripts.md");
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..1024 {
        file.write_all(&[b'a'; 4096]).unwrap(); // 实际4MiB磁盘输入，堆上只保留固定4KiB写块。
    }
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let before_reads = mapping.reclaim_attempts();
    let mut job =
        MarkdownScriptJob::new(source, &authority, MarkdownScriptCancellation::new()).unwrap();
    let receipt = job.resource_observer();
    drop(document);
    let work = finish(&mut job, &authority, ONE);
    assert_eq!(work.polls, 2);
    assert_eq!(
        (work.bytes, work.scalars, work.records, work.stack_ops),
        (0, 0, 1, 2)
    );
    assert_eq!(mapping.reclaim_attempts(), before_reads);
    let profile = job.storage_profile();
    assert!(profile.printable_summary_used);
    assert_eq!(profile.source_bytes_read, 0);
    assert_eq!(profile.source_borrow_max_bytes, 0);
    assert_eq!((profile.completed_runs, profile.bracket_records), (1, 0));
    let actual = runs(&mut job, &authority);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].end_byte, 4 * 1024 * 1024);
    assert_eq!(actual[0].end_utf16, actual[0].end_byte);
    assert_eq!(actual[0].script.iso15924_tag(), *b"Latn");
    job.close().unwrap();
    assert_released(&receipt);
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn unicode_controls_and_rtl_text_keep_original_source_work_and_machine_results() {
    for text in [
        "a中",
        "אב (12)",
        "a\t12",
        "a\r\nb",
        "a\0b",
        "a\u{2067}ב\u{2069}",
    ] {
        let (_directory, document) = fixture(text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let mut actual = MarkdownScriptJob::new(
            source.clone(),
            &authority,
            MarkdownScriptCancellation::new(),
        )
        .unwrap();
        let mut oracle =
            MarkdownScriptJob::new_streaming(source, &authority, MarkdownScriptCancellation::new())
                .unwrap();
        assert!(!actual.storage_profile().printable_summary_used);
        let work = finish(&mut actual, &authority, ONE);
        finish(&mut oracle, &authority, ONE);
        assert_eq!(work.bytes, text.len());
        assert_eq!(work.scalars, text.chars().count());
        assert_eq!(runs(&mut actual, &authority), runs(&mut oracle, &authority));
    }
}

#[test]
fn frozen_edited_restored_roots_and_foreign_capture_keep_exact_source_authority() {
    let (_directory, mut document) = fixture("[]abc");
    let old = MarkdownLayoutTextSource::capture(&document);
    let old_authority = old.authority();
    document
        .insert_utf16(2, "א", MarkdownTextSelection::caret(3))
        .unwrap();
    let edited = MarkdownLayoutTextSource::capture(&document);
    let edited_authority = edited.authority();
    assert!(old_authority != edited_authority);
    let mut old_job =
        MarkdownScriptJob::new(old, &old_authority, MarkdownScriptCancellation::new()).unwrap();
    assert_eq!(finish(&mut old_job, &old_authority, ONE).bytes, 0);
    let mut edited_job =
        MarkdownScriptJob::new(edited, &edited_authority, MarkdownScriptCancellation::new())
            .unwrap();
    assert!(!edited_job.storage_profile().printable_summary_used);
    assert_eq!(finish(&mut edited_job, &edited_authority, ONE).bytes, 7);
    assert!(document.undo().is_some());
    let restored = MarkdownLayoutTextSource::capture(&document);
    let restored_authority = restored.authority();
    assert!(restored_authority != old_authority);
    let foreign = MarkdownLayoutTextSource::capture(&document);
    let mut restored_job = MarkdownScriptJob::new(
        restored,
        &restored_authority,
        MarkdownScriptCancellation::new(),
    )
    .unwrap();
    assert_eq!(finish(&mut restored_job, &restored_authority, ONE).bytes, 0);
    assert!(matches!(
        restored_job.run_at(&foreign.authority(), MarkdownScriptUnit::Byte, 0),
        Err(MarkdownScriptError::SourceMismatch)
    )); // 同字节/root/generation的外来capture仍撤销本job，不能借摘要混票。
    assert!(matches!(
        restored_job.run_at(&restored_authority, MarkdownScriptUnit::Byte, 0),
        Err(MarkdownScriptError::Poisoned)
    ));
}

#[test]
fn cancellation_between_actual_phases_and_after_ready_releases_both_files_and_source() {
    for phase in 0..4 {
        let (_directory, document) = fixture("[]abc");
        let mapping = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let payload = source.resource_observer();
        let cancel = MarkdownScriptCancellation::new();
        if phase == 0 {
            cancel.cancel();
            assert!(matches!(
                MarkdownScriptJob::new(source, &authority, cancel),
                Err(MarkdownScriptError::Cancelled)
            ));
        } else {
            let mut job = MarkdownScriptJob::new(source, &authority, cancel.clone()).unwrap();
            let receipt = job.resource_observer();
            if phase == 2 {
                assert_eq!(
                    job.step(&authority, ONE).unwrap().status,
                    MarkdownScriptStatus::Pending
                );
            } else if phase == 3 {
                finish(&mut job, &authority, ONE);
            }
            cancel.cancel();
            assert!(matches!(
                job.step(&authority, ONE),
                Err(MarkdownScriptError::Cancelled)
            ));
            assert!(matches!(
                job.run_at(&authority, MarkdownScriptUnit::Byte, 0),
                Err(MarkdownScriptError::Cancelled)
            ));
            job.close().unwrap(); // 取消不能跳过任何真实FD或残留写映射的清理。
            assert_released(&receipt);
        }
        drop(document);
        assert!(payload.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn real_source_eio_precedes_cancel_foreign_authority_and_completed_summary() {
    for phase in 0..3 {
        let (_directory, document) = fixture("[]abc");
        let mapping = document.original_backing_observer().unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let cancel = MarkdownScriptCancellation::new();
        let mut job = MarkdownScriptJob::new(source.clone(), &authority, cancel.clone()).unwrap();
        let receipt = job.resource_observer();
        if phase == 1 {
            assert_eq!(
                job.step(&authority, ONE).unwrap().status,
                MarkdownScriptStatus::Pending
            );
        } else if phase == 2 {
            finish(&mut job, &authority, ONE);
        }
        source.visit_byte_range(0..1, |_| Ok(())).unwrap();
        document.fail_next_original_reclaim_for_test();
        assert_eq!(
            document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ); // 真实借入页再触发驱逐错误；不是只置flag就宣称来源已坏。
        cancel.cancel();
        let foreign = MarkdownLayoutTextSource::capture(&document).authority();
        assert!(matches!(
            job.step(&foreign, ONE),
            Err(MarkdownScriptError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert!(matches!(
            job.run_at(&foreign, MarkdownScriptUnit::Byte, 0),
            Err(MarkdownScriptError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        job.close().unwrap();
        drop(source);
        drop(document);
        assert_released(&receipt);
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
        assert_eq!(mapping.first_errno(), Some(libc::EIO));
    }
}

#[test]
fn summary_private_seal_does_not_write_but_actual_cross_page_failure_revokes_known_runs() {
    let (directory, document) = fixture("[]abc");
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mut job =
        MarkdownScriptJob::new(source, &authority, MarkdownScriptCancellation::new()).unwrap();
    let path = directory.path().join("readonly-summary-run");
    std::fs::write(&path, []).unwrap();
    job.store =
        store::Store::with_runs_file(std::fs::File::open(&path).unwrap(), &job.cancellation)
            .unwrap();
    let receipt = job.resource_observer();
    // 原精确摘要只有一条64B run，既没有跨页也不交出File Reader；private seal
    // 合法读取原buffer，不能为延续旧测试预期强制把这条临时记录写盘。
    let work = finish(&mut job, &authority, ONE);
    assert_eq!(
        (work.bytes, work.scalars, work.records, work.stack_ops),
        (0, 0, 1, 2)
    );
    assert!(job.machine.ready && job.machine.byte == job.source.byte_len());
    assert!(job.storage_profile().printable_summary_used);
    assert_eq!(job.storage_profile().source_bytes_read, 0);
    assert!(matches!(
        job.run_at(&authority, MarkdownScriptUnit::Byte, 0).unwrap(),
        MarkdownScriptAnswer::Known(Some(run)) if run.start_byte == 0 && run.end_byte == 5
    ));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    assert_eq!(
        receipt.snapshot().buffer_bytes,
        MARKDOWN_SCRIPT_INDEX_BYTES / 2
    );
    drop(document);
    job.close().unwrap();
    assert_released(&receipt);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

    // 真正需要File写入的位置是原SCX连续run跨页。混合脚本保留原Machine，
    // 先取得一个真实Known前run，再由只读FD的真实EBADF撤销旧查询权限；不造摘要fallback。
    let text = "aא".repeat(300);
    let (directory, document) = fixture(&text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mut job =
        MarkdownScriptJob::new(source, &authority, MarkdownScriptCancellation::new()).unwrap();
    let path = directory.path().join("readonly-cross-page-runs");
    std::fs::write(&path, []).unwrap();
    job.store =
        store::Store::with_runs_file(std::fs::File::open(&path).unwrap(), &job.cancellation)
            .unwrap();
    let receipt = job.resource_observer();
    let mut known_prefix = false;
    let mut failure = None;
    for _ in 0..text.len() * 8 + 32 {
        match job.step(&authority, ONE) {
            Ok(step) => {
                assert!(step.bytes <= ONE.bytes && step.scalars <= ONE.scalars);
                assert!(step.records <= ONE.records && step.stack_ops <= ONE.stack_ops);
                assert_eq!(step.status, MarkdownScriptStatus::Pending);
                if !known_prefix && job.store.run_count() != 0 {
                    assert!(matches!(
                        job.run_at(&authority, MarkdownScriptUnit::Byte, 0).unwrap(),
                        MarkdownScriptAnswer::Known(Some(run)) if run.start_byte == 0 && run.end_byte == 1
                    ));
                    known_prefix = true;
                }
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    assert!(known_prefix);
    assert!(
        matches!(failure, Some(MarkdownScriptError::Storage(error)) if error.raw_os_error() == Some(libc::EBADF))
    );
    assert!(!job.machine.ready && job.machine.byte < text.len());
    assert!(!job.storage_profile().printable_summary_used);
    assert!(job.storage_profile().source_bytes_read > 0);
    assert!(matches!(
        job.run_at(&authority, MarkdownScriptUnit::Byte, 0),
        Err(MarkdownScriptError::Poisoned)
    ));
    assert!(matches!(
        job.step(&authority, ONE),
        Err(MarkdownScriptError::Poisoned)
    ));
    drop(document);
    assert!(matches!(job.close(), Err(MarkdownScriptError::Poisoned)));
    let closed = receipt.snapshot();
    assert!(
        !closed.run_file_open
            && !closed.stack_file_open
            && closed.buffer_bytes == 0
            && closed.mapped_bytes == 0
    );
    assert!(closed.cleanup_failed && closed.source_payload_released);
    assert_eq!(
        closed.run_peak_buffer_bytes,
        MARKDOWN_SCRIPT_INDEX_BYTES / 2
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
}

#[path = "suffix_tests.rs"]
mod suffix_tests;
