//! 折叠只跳过真实硬控制之间的隐区；独立端点 oracle 同时核对可见序号和实际读取。

use super::{self as tests, *};
use crate::markdown_layout_source::folds::FoldPlan;
use std::ops::Range;

/// 推进真实 job 到封存；每个 Pending 都应消费字节、标量或记录，不能靠空轮询假完成。
fn finish_folded(
    job: &mut MarkdownGraphemeJob,
    authority: &MarkdownLayoutTextAuthority,
    budget: MarkdownGraphemeBudget,
) {
    for _ in 0..512 {
        let step = job.step(authority, budget).unwrap();
        assert!(step.bytes <= budget.bytes);
        assert!(step.scalars <= budget.scalars);
        assert!(step.records <= budget.records);
        if step.status == MarkdownGraphemeStatus::Ready {
            return;
        }
        assert!(step.bytes != 0 || step.scalars != 0 || step.records != 0);
    }
    panic!("有限可见内容没有完成真实 EOF");
}

/// 使用原 Source 的固定折叠证明；fixture 的 mmap 和索引仍由原资源 owner 管理。
fn folded_job(source: &MarkdownLayoutTextSource, hidden: Range<usize>) -> MarkdownGraphemeJob {
    let plan = FoldPlan::new(source, &[hidden]).unwrap();
    let mut job = MarkdownGraphemeJob::new(source.clone()).unwrap();
    job.install_fold_plan(plan).unwrap();
    job
}

#[test]
fn folded_giant_unicode_body_reads_only_real_control_edges_and_visible_text() {
    let header = "# 中";
    let hidden_body = "👩\u{200d}💻\u{301}".repeat(8192);
    let suffix = "A\u{301}🇨🇳Z";
    let text = format!("{header}\r\n{hidden_body}\r\n{suffix}");
    let source = tests::source(text.as_bytes());
    let authority = source.authority();
    let end = MarkdownGraphemeBoundary {
        byte: header.len() + 2 + hidden_body.len() + 2,
        utf16: header.encode_utf16().count() + 2 + hidden_body.encode_utf16().count() + 2,
    };
    let mut job = folded_job(&source, 3..end.utf16);
    // 此拒绝既不能误报非边界，也不能把健康 owner 撤权；随后仍须完成同一 job。
    for query in [
        MarkdownGraphemeQuery::IsBoundary,
        MarkdownGraphemeQuery::Previous,
        MarkdownGraphemeQuery::Next,
    ] {
        assert!(matches!(
            job.query(&authority, MarkdownGraphemeUnit::Utf16, 5, query),
            Err(MarkdownGraphemeError::FoldedRange)
        ));
    }
    finish_folded(&mut job, &authority, tests::TINY);
    let expected = [
        MarkdownGraphemeBoundary { byte: 0, utf16: 0 },
        MarkdownGraphemeBoundary { byte: 1, utf16: 1 },
        MarkdownGraphemeBoundary { byte: 2, utf16: 2 },
        MarkdownGraphemeBoundary { byte: 5, utf16: 3 },
        end,
        MarkdownGraphemeBoundary {
            byte: end.byte + 3,
            utf16: end.utf16 + 2,
        },
        MarkdownGraphemeBoundary {
            byte: end.byte + 11,
            utf16: end.utf16 + 6,
        },
        MarkdownGraphemeBoundary {
            byte: end.byte + 12,
            utf16: end.utf16 + 7,
        },
    ]; // A×acute、RI×RI 的真实后缀规则不受被跳过的 emoji/Indic 状态污染。
    let mut prefix = job.stable_prefix(&authority).unwrap();
    let facts = prefix.facts().unwrap();
    assert_eq!(
        facts.ordinal_domain,
        MarkdownGraphemeOrdinalDomain::FoldedVisible
    );
    assert_eq!(facts.total_count, Some(expected.len()));
    let mut actual = [MarkdownGraphemeBoundary { byte: 0, utf16: 0 }; 8];
    assert_eq!(
        prefix.read_boundaries(0..8, &mut actual).unwrap(),
        MarkdownGraphemePrefixRead::Read
    );
    assert_eq!(actual, expected);
    assert_eq!(
        job.storage_profile().source_bytes_read,
        header.len() + 2 + 2 + suffix.len()
    );
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            3,
            MarkdownGraphemeQuery::Next
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Known(Some(end))
    );
    assert_eq!(
        job.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            end.utf16,
            MarkdownGraphemeQuery::Previous
        )
        .unwrap(),
        MarkdownGraphemeAnswer::Known(Some(expected[3]))
    );
    let observer = job.resource_observer();
    job.close().unwrap();
    tests::released(&observer, false);
}

#[test]
fn folded_one_byte_crlf_and_real_eof_never_publish_hidden_control_ordinals() {
    let one = MarkdownGraphemeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
    };
    for (text, hidden, expected_reads, expected) in [
        (
            "H\r\n中\r\n\u{301}Z",
            1..6,
            8,
            vec![(0, 0), (1, 1), (8, 6), (10, 7), (11, 8)],
        ),
        ("H\r\n中", 1..4, 3, vec![(0, 0), (1, 1), (6, 4)]),
        ("H\r\n中\r\n", 1..6, 5, vec![(0, 0), (1, 1), (8, 6)]),
        ("H\r\nZ", 1..3, 4, vec![(0, 0), (1, 1), (3, 3), (4, 4)]),
        ("H\r中\nZ", 1..4, 4, vec![(0, 0), (1, 1), (6, 4), (7, 5)]),
    ] {
        let source = tests::source(text.as_bytes());
        let authority = source.authority();
        let mut job = folded_job(&source, hidden);
        finish_folded(&mut job, &authority, one);
        let mut prefix = job.stable_prefix(&authority).unwrap();
        let facts = prefix.facts().unwrap();
        assert_eq!(facts.total_count, Some(expected.len()), "{text:?}");
        let mut actual = vec![MarkdownGraphemeBoundary { byte: 0, utf16: 0 }; expected.len()];
        prefix
            .read_boundaries(0..expected.len(), &mut actual)
            .unwrap();
        let expected = expected
            .into_iter()
            .map(|(byte, utf16)| MarkdownGraphemeBoundary { byte, utf16 })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{text:?}");
        assert_eq!(
            job.storage_profile().source_bytes_read,
            expected_reads,
            "{text:?}"
        );
        // 同一 Source 的普通 job 仍分析完整原文，并保持全源 ordinal；折叠不改全局协议。
        let mut ordinary = MarkdownGraphemeJob::new(source.clone()).unwrap();
        tests::finish(&mut ordinary, &authority);
        assert_eq!(
            ordinary
                .stable_prefix(&authority)
                .unwrap()
                .facts()
                .unwrap()
                .ordinal_domain,
            MarkdownGraphemeOrdinalDomain::Source
        );
    }
}

#[test]
fn folded_plan_keeps_exact_source_identity_cancel_and_first_resource_failure() {
    let (directory, document) = tests::fixture("H\n中\nZ".as_bytes());
    let source = MarkdownLayoutTextSource::capture(&document);
    let foreign = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let mut foreign_plan_job = MarkdownGraphemeJob::new(source.clone()).unwrap();
    assert!(matches!(
        foreign_plan_job
            .install_fold_plan(FoldPlan::new(&foreign, std::slice::from_ref(&(1..4))).unwrap()),
        Err(MarkdownGraphemeError::SourceMismatch)
    ));
    let mut cancelled = folded_job(&source, 1..4);
    cancelled.cancel();
    assert!(matches!(
        cancelled.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            2,
            MarkdownGraphemeQuery::IsBoundary
        ),
        Err(MarkdownGraphemeError::Poisoned)
    ));
    let mut failed = folded_job(&source, 1..4);
    tests::poison_original_read_windows(&document);
    // 同原 mmap 的真实首错优先于“隐藏范围”拒绝，不能借可见索引或取消状态掩盖 EIO。
    assert!(matches!(
        failed.query(
            &authority,
            MarkdownGraphemeUnit::Utf16,
            2,
            MarkdownGraphemeQuery::IsBoundary
        ),
        Err(MarkdownGraphemeError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    drop((
        failed,
        cancelled,
        foreign_plan_job,
        foreign,
        source,
        document,
        directory,
    ));
}
