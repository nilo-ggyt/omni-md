//! Folds skip only hidden regions between actual hard controls. Independent endpoint oracles
//! check visible-domain ordinals and actual source reads together.

use super::{self as tests, *};
use crate::markdown_layout_source::folds::FoldPlan;
use std::ops::Range;

/// Advance the real job through sealing; each Pending step consumes bytes, scalars, or records instead of empty polling.
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

/// Use the original Source's fold proof while its actual resource owners retain the fixture mapping and index.
fn folded_job(source: &MarkdownLayoutTextSource, hidden: Range<usize>) -> MarkdownGraphemeJob {
    let plan = FoldPlan::new(source, &[hidden]).unwrap();
    let mut job = MarkdownGraphemeJob::new(source.clone()).unwrap();
    job.install_fold_plan(plan).unwrap();
    job
}

#[test]
fn folded_giant_unicode_body_reads_only_real_control_edges_and_visible_text() {
    // The large hidden body is finite fixture data. Its independently supplied physical
    // controls allow skipping the body while preserving exact global visible endpoints.
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
    // A hidden-domain query must neither pretend NoBreak nor revoke this healthy owner;
    // the same original job must remain usable at visible positions.
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
    ]; // Actual A-plus-acute and paired-RI suffix rules are not polluted by skipped emoji/Indic context.
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
    // One-byte quanta split CRLF and Unicode scalars. The explicit expectations cover
    // closing controls, unterminated real EOF, and folds containing only their opening edge.
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
        // An ordinary job on the same Source still scans all text with whole-source
        // ordinals; the folded display domain does not redefine the global protocol.
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
    // A separate capture cannot supply the fold proof even for the same document.
    // Cancellation and actual original-source failure retain their distinct outcomes.
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
    // The original mmap's real failure precedes hidden-domain rejection; visible records
    // or cancellation state cannot conceal its EIO.
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
