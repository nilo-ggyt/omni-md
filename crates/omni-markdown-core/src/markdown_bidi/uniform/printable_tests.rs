//! 精确整源ASCII证书的语义、读取预算与原资源收据；有限UBA oracle不进入产品路径。

use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownLayoutTextAuthority, MarkdownLayoutTextSource,
    MarkdownTextSelection,
};
use std::{
    future::Future,
    io::Write,
    task::{Context, Poll, Waker},
};
use unicode_bidi::utf16::BidiInfo;

const ONE: MarkdownBidiPrepareBudget = MarkdownBidiPrepareBudget {
    bytes: 1,
    scalars: 1,
    metadata_ops: 1,
};

struct Fixture {
    _directory: tempfile::TempDir,
    document: MarkdownDocumentBuffer,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("printable.md");
        std::fs::write(&path, text).unwrap();
        Self {
            document: MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap(),
            _directory: directory,
        }
    }

    fn source(&self) -> MarkdownLayoutTextSource {
        MarkdownLayoutTextSource::capture(&self.document)
    }
}

fn job(
    source: MarkdownLayoutTextSource,
    base: BidiBaseDirection,
    cancel: &MarkdownBidiCancellation,
) -> (
    MarkdownLayoutTextAuthority,
    MarkdownBidiPrepareJob,
    MarkdownBidiResourceObserver,
) {
    // authority先从原payload取得，再把唯一Source移入Future；不另capture同文本补权限。
    let authority = source.authority();
    let (result, receipt) = MarkdownBidiPrepareJob::new_layout_source_observed(
        source,
        &authority,
        base,
        BidiMemoryBudget::new(65536).unwrap(),
        cancel,
    );
    (authority, result.unwrap(), receipt)
}

fn assert_quantum(step: MarkdownBidiPrepareStep) {
    assert!(step.bytes <= ONE.bytes);
    assert!(step.scalars <= ONE.scalars);
    assert!(step.metadata_ops <= ONE.metadata_ops);
    assert_eq!(
        step.metadata_ops,
        step.record_reads + step.record_writes + step.resource_ops
    );
    assert!(step.source_borrow_max_bytes <= ONE.bytes);
}

fn fast_ready(
    job: &mut MarkdownBidiPrepareJob,
    authority: &MarkdownLayoutTextAuthority,
) -> MarkdownBidiParagraph {
    let step = job.step(authority, ONE).unwrap();
    assert_eq!(step.status, MarkdownBidiPrepareStatus::Ready);
    // 真实Gate记录正文借用和每项实际操作；整源证书应在首个量子完成且无Reader/Store工作。
    assert_eq!(
        (
            step.bytes,
            step.scalars,
            step.metadata_ops,
            step.record_reads,
            step.record_writes,
            step.resource_ops,
            step.source_borrow_max_bytes,
        ),
        (0, 0, 0, 0, 0, 0, 0)
    );
    let actual = job.take_ready(authority).unwrap().unwrap();
    assert!(matches!(actual.resolution, Resolution::UniformZero(_)));
    assert_eq!(actual.storage_profile().source_borrow_max_bytes, 0);
    assert_eq!(actual.storage_profile().cache_bytes, 0);
    assert_eq!(actual.storage_profile().page_loads, 0);
    actual
}

fn assert_unallocated(receipt: &MarkdownBidiResourceObserver) {
    let actual = receipt.snapshot();
    assert!(!actual.file_created && !actual.file_open && !actual.close_succeeded);
    assert_eq!(actual.close_errno, None);
    assert_eq!((actual.cache_bytes, actual.peak_cache_bytes), (0, 0));
    assert!(receipt.is_released()); // 从未分配须与真实close成功区别，不能制造关闭事件。
}

fn assert_ascii_oracle(text: &str, base: BidiBaseDirection, actual: &mut MarkdownBidiParagraph) {
    assert!(text.is_ascii() && text.len() <= 4096);
    // 只有此有限oracle建立完整数组；产品算法与4MiB夹具不建立全文副本。
    let units: Vec<u16> = text.encode_utf16().collect();
    let oracle = BidiInfo::new_with_data_source(
        &UnicodeBidiData,
        &units,
        base.level()
            .map(|level| unicode_bidi::Level::new(level).unwrap()),
    );
    assert_eq!(actual.actual_paragraph_count(), oracle.paragraphs.len());
    for (position, expected) in oracle.levels.iter().enumerate() {
        assert_eq!(actual.level_at_utf16(position).unwrap(), expected.number());
    }
    for paragraph in &oracle.paragraphs {
        assert_eq!(
            actual
                .paragraph_range_at_utf16(paragraph.range.start)
                .unwrap(),
            Some(paragraph.range.clone())
        );
        assert_eq!(
            actual
                .paragraph_base_level_at_utf16(paragraph.range.start)
                .unwrap(),
            Some(paragraph.level.number())
        );
        let levels = oracle.reordered_levels_per_char(paragraph, paragraph.range.clone());
        let expected: Vec<_> = BidiInfo::reorder_visual(&levels[paragraph.range.clone()])
            .into_iter()
            .map(|index| paragraph.range.start + index)
            .collect();
        let mut observed = Vec::new();
        actual
            .visit_visual_runs(paragraph.range.clone(), |run| {
                // ASCII一个单位就是一个scalar；真实L1/L2逆序仍按run level应用。
                let range = run.source_utf16_start..run.source_utf16_end;
                if run.level & 1 == 0 {
                    observed.extend(range);
                } else {
                    observed.extend(range.rev());
                }
            })
            .unwrap();
        assert_eq!(observed, expected);
    }
    assert_eq!(actual.paragraph_range_at_utf16(text.len()).unwrap(), None);
}

#[test]
fn exact_full_ascii_is_zero_read_and_matches_complete_uba_oracle() {
    let all_printable: String = (b' '..=b'~').map(char::from).collect();
    for text in [
        "",
        "1234 + 56.78, ($9) [10] {11} <> -- / :!?",
        "   () [] {} \"' -+ 123 %$ #@   ",
        "a [b (12)] {c} !",
        &all_printable,
    ] {
        for base in [BidiBaseDirection::Auto, BidiBaseDirection::LeftToRight] {
            let fixture = Fixture::new(text);
            let (authority, mut preparation, receipt) =
                job(fixture.source(), base, &MarkdownBidiCancellation::new());
            let mut actual = fast_ready(&mut preparation, &authority);
            assert_ascii_oracle(text, base, &mut actual);
            actual.require_layout_source(&authority).unwrap();
            assert_unallocated(&actual.resource_observer());
            drop(actual);
            drop(preparation);
            assert_unallocated(&receipt);
        }
    }
}

#[test]
fn four_mebibyte_single_paragraph_needs_no_uba_source_scan_or_metadata_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large-printable.md");
    let mut file = std::fs::File::create(&path).unwrap();
    for _ in 0..1024 {
        // 固定4KiB写入实际4MiB文件，不用repeat String/全文Vec代替懒加载工作负载。
        file.write_all(&[b'a'; 4096]).unwrap();
    }
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 8 * 1024 * 1024).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let payload = source.resource_observer();
    let mapping = document.original_backing_observer().unwrap();
    let reclaim_before = mapping.reclaim_attempts();
    let (authority, mut preparation, receipt) = job(
        source,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    drop(document); // 后续只有原准备Future/完成值拥有冻结Source，测试不靠活动Doc兜底。
    let mut actual = fast_ready(&mut preparation, &authority);
    assert_eq!(mapping.reclaim_attempts(), reclaim_before);
    assert_eq!(actual.source_range(), 0..4 * 1024 * 1024);
    for position in [0, 2 * 1024 * 1024, 4 * 1024 * 1024 - 1] {
        assert_eq!(actual.level_at_utf16(position).unwrap(), 0);
        assert_eq!(
            actual.paragraph_range_at_utf16(position).unwrap(),
            Some(0..4 * 1024 * 1024)
        );
    }
    assert!(!payload.payload_released());
    assert_unallocated(&receipt);
    drop(actual);
    drop(preparation);
    assert!(payload.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
fn frozen_root_proof_survives_edit_but_never_authorizes_new_or_foreign_source() {
    let mut fixture = Fixture::new("abcdef (123)");
    let old = fixture.source();
    let old_authority = old.authority();
    fixture
        .document
        .insert_utf16(3, "א", MarkdownTextSelection::caret(4))
        .unwrap();
    let edited = fixture.source();
    assert!(old_authority != edited.authority());
    let (old_authority, mut old_job, _) = job(
        old,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let mut old_actual = fast_ready(&mut old_job, &old_authority);
    assert_ascii_oracle("abcdef (123)", BidiBaseDirection::Auto, &mut old_actual);
    let (edited_authority, mut edited_job, _) = job(
        edited,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let first = edited_job.step(&edited_authority, ONE).unwrap();
    assert_quantum(first);
    assert_eq!(first.status, MarkdownBidiPrepareStatus::Pending);
    assert_eq!(first.bytes, 1); // 后继真实混合root失去证书，不能复用旧root的全零结论。
    assert!(fixture.document.undo().is_some());
    let restored = fixture.source();
    let restored_authority = restored.authority();
    assert!(old_authority != restored_authority);
    let foreign_same_root = fixture.source();
    let (restored_authority, mut restored_job, _) = job(
        restored,
        BidiBaseDirection::Auto,
        &MarkdownBidiCancellation::new(),
    );
    let restored_actual = fast_ready(&mut restored_job, &restored_authority);
    restored_actual
        .require_layout_source(&restored_authority)
        .unwrap();
    assert!(matches!(
        restored_actual.require_layout_source(&old_authority),
        Err(BidiError::SourceMismatch)
    ));
    assert!(matches!(
        restored_actual.require_layout_source(&foreign_same_root.authority()),
        Err(BidiError::SourceMismatch)
    )); // 同generation/同root/同字节仍不等于同一次原Source capture。
}

#[test]
fn rtl_paragraphs_and_unproven_unicode_keep_original_cooperative_reader() {
    for (text, base) in [
        ("abc (123)", BidiBaseDirection::RightToLeft),
        ("abc\nsecond", BidiBaseDirection::Auto),
        ("abc\r123", BidiBaseDirection::LeftToRight),
        ("abcא", BidiBaseDirection::Auto),
    ] {
        let fixture = Fixture::new(text);
        let (authority, mut preparation, _) =
            job(fixture.source(), base, &MarkdownBidiCancellation::new());
        let first = preparation.step(&authority, ONE).unwrap();
        assert_quantum(first);
        if text == "abc\nsecond" || text == "abc\r123" {
            assert_eq!(first.status, MarkdownBidiPrepareStatus::Ready);
            let mut actual = preparation.take_ready(&authority).unwrap().unwrap();
            assert_ascii_oracle(text, base, &mut actual);
            continue; // 真B段仍进完整oracle；只不再为已证明level0的源建立全部Text记录。
        }
        assert_eq!(first.status, MarkdownBidiPrepareStatus::Pending);
        assert!(preparation.take_ready(&authority).unwrap().is_none());
        if base == BidiBaseDirection::RightToLeft {
            // RTL立即拒绝Uniform，原Store先创建FD，首量子尚无正文读取；不得为满足
            // 测试期待而改写资源/读取顺序。随后完成原算法并与独立整段UBA语义对照。
            assert_eq!((first.bytes, first.source_borrow_max_bytes), (0, 0));
            assert_eq!((first.metadata_ops, first.resource_ops), (1, 1));
            let mut completed = false;
            for _ in 0..100_000 {
                let step = preparation.step(&authority, ONE).unwrap();
                assert_quantum(step);
                if step.status == MarkdownBidiPrepareStatus::Ready {
                    let mut actual = preparation.take_ready(&authority).unwrap().unwrap();
                    assert!(matches!(actual.resolution, Resolution::Paged(_)));
                    assert_ascii_oracle(text, base, &mut actual);
                    completed = true;
                    break;
                }
            }
            assert!(completed, "有限RTL输入未在原量子推进中完成");
        } else {
            assert_eq!((first.bytes, first.source_borrow_max_bytes), (1, 1));
        }
    }
}

#[test]
fn partial_ranges_read_only_original_one_byte_quanta_even_inside_mixed_leaf() {
    for (text, source, bytes) in [("abcd", 1..3, 1..3), ("aaa中bbb", 4..7, 6..9)] {
        let fixture = Fixture::new(text);
        let snapshot = fixture.document.text_snapshot();
        let gate = Gate::new(&snapshot, MarkdownBidiCancellation::new());
        let mut preparation = Box::pin(certify_prepared(
            &snapshot,
            &source,
            bytes.clone(),
            BidiBaseDirection::Auto,
            &gate,
        ));
        let mut context = Context::from_waker(Waker::noop());
        gate.replenish(ONE);
        assert!(preparation.as_mut().poll(&mut context).is_pending());
        let first = gate.progress();
        assert_quantum(first);
        assert_eq!((first.bytes, first.scalars), (1, 1));
        let mut total_bytes = first.bytes;
        let mut done = false;
        for _ in 0..16 {
            gate.replenish(ONE);
            let answer = preparation.as_mut().poll(&mut context);
            let step = gate.progress();
            assert_quantum(step);
            total_bytes += step.bytes;
            if let Poll::Ready(answer) = answer {
                assert!(answer.unwrap().is_some());
                done = true;
                break;
            }
        }
        assert!(done); // Partial必须读完其真实范围，不能以第一次Pending当作完成。
        assert_eq!(total_bytes, bytes.len());
        assert_eq!(gate.source_borrow_max(), 1);
    }
}

#[test]
fn cancellation_still_revokes_before_and_after_zero_read_certificate() {
    for phase in 0..3 {
        let fixture = Fixture::new("abc (123)");
        let source = fixture.source();
        let authority = source.authority();
        let payload = source.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        let cancel = MarkdownBidiCancellation::new();
        if phase == 0 {
            cancel.cancel(); // 构造前取消也不能借精确聚合制造一份新证书。
        }
        let (result, receipt) = MarkdownBidiPrepareJob::new_layout_source_observed(
            source,
            &authority,
            BidiBaseDirection::Auto,
            BidiMemoryBudget::new(65536).unwrap(),
            &cancel,
        );
        if phase == 0 {
            assert!(matches!(result, Err(BidiError::Cancelled)));
        } else {
            let mut preparation = result.unwrap();
            if phase == 1 {
                cancel.cancel(); // 已捕Source但尚未poll时取消，仍应退出原Future而不借正文。
                assert!(matches!(
                    preparation.step(&authority, ONE),
                    Err(BidiError::Cancelled)
                ));
                assert_eq!(preparation.last_step().bytes, 0);
            } else {
                let mut actual = fast_ready(&mut preparation, &authority);
                cancel.cancel();
                assert!(matches!(
                    actual.level_at_utf16(0),
                    Err(BidiError::Cancelled)
                ));
            }
            drop(preparation);
        }
        drop(fixture);
        assert_unallocated(&receipt);
        assert!(payload.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
fn actual_backing_eio_precedes_cancel_foreign_authority_and_ready_cache() {
    for ready in [false, true] {
        let fixture = Fixture::new("abc (123)");
        let source = fixture.source();
        let payload = source.resource_observer();
        let mapping = fixture.document.original_backing_observer().unwrap();
        let cancel = MarkdownBidiCancellation::new();
        let (authority, mut preparation, receipt) =
            job(source.clone(), BidiBaseDirection::Auto, &cancel);
        if ready {
            let first = preparation.step(&authority, ONE).unwrap();
            assert_eq!(first.status, MarkdownBidiPrepareStatus::Ready);
            assert_eq!(first.bytes, 0); // 保留真实Ready缓存，随后实际源故障必须撤销它。
        }
        source.visit_byte_range(0..1, |_| Ok(())).unwrap();
        fixture.document.fail_next_original_reclaim_for_test();
        assert_eq!(
            fixture.document.reclaim_original_read_windows_for_test(),
            Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                libc::EIO
            ))
        ); // 只有原页实际驱逐失败才算EIO；设置测试位本身不算来源失败。
        cancel.cancel();
        let foreign = fixture.source().authority();
        assert!(matches!(
            preparation.step(&foreign, ONE),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert!(matches!(
            preparation.take_ready(&foreign),
            Err(BidiError::Source(
                MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
            ))
        ));
        assert_unallocated(&receipt);
        drop(preparation);
        drop(source);
        drop(fixture);
        assert!(payload.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0);
        assert_eq!(mapping.first_errno(), Some(libc::EIO));
    }
}
