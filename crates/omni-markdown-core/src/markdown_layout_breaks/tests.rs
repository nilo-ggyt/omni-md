//! Default line opportunities may lie inside actual graphemes; intersection queries preserve Pending, finite yields, and original cursor identity.
use super::*;

fn source(text: &str) -> (tempfile::TempDir, MarkdownLayoutTextSource) {
    // Capture one actual immutable Source from a private synthetic file; returning it retains its legitimate backing owner after the document drops.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("layout-breaks.md");
    std::fs::write(&path, text).unwrap();
    let document = crate::MarkdownDocumentBuffer::open(&path, 65_536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    (directory, source)
}

fn finish(job: &mut MarkdownLayoutBreakJob, authority: &MarkdownLayoutTextAuthority) {
    // Both algorithms together receive only one byte/scalar/record per turn, including progress across UTF-8 fragments and actual EOF.
    for _ in 0..100_000 {
        let step = job
            .step(
                authority,
                MarkdownLineBreakBudget {
                    bytes: 1,
                    scalars: 1,
                    records: 1,
                },
            )
            .unwrap();
        assert!(step.bytes <= 1 && step.scalars <= 1 && step.records <= 1);
        if step.ready {
            return;
        }
    }
    panic!("有限合成源的双阶段没有完成");
}

#[test]
// Hundreds of actual raw line opportunities inside extended graphemes must yield rather than become a single unbounded intersection query.
fn raw_default_break_inside_extended_grapheme_is_rejected_without_an_unbounded_query() {
    let text = " \u{301}".repeat(257);
    let (_directory, source) = source(&text);
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    finish(&mut job, &authority);
    let mut at = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            1,
            MarkdownLayoutBreakQuery::At,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut at, 1).unwrap(),
        MarkdownLayoutBreakAnswer::Known(None)
    );
    assert!(matches!(
        job.query(&mut at, 1),
        Err(MarkdownLayoutBreakError::CursorMismatch)
    ));
    let mut next = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            0,
            MarkdownLayoutBreakQuery::Next,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut next, 1).unwrap(),
        MarkdownLayoutBreakAnswer::Yielded
    );
    let mut yielded = 1;
    loop {
        match job.query(&mut next, 1).unwrap() {
            MarkdownLayoutBreakAnswer::Yielded => yielded += 1,
            MarkdownLayoutBreakAnswer::Known(Some(boundary)) => {
                assert_eq!(boundary.utf16, source.utf16_len());
                assert_eq!(boundary.byte, source.byte_len());
                assert_eq!(boundary.kind, crate::MarkdownLineBreakKind::Mandatory);
                break;
            }
            other => panic!("完整源的后继查询返回了错误状态 {other:?}"),
        }
    }
    assert!(yielded >= 256); // Numerous raw opportunities lie inside graphemes and cannot all be skipped secretly within one turn.
}

#[test]
// A known line candidate with unfinished GCB coverage remains the same candidate when the original algorithms later make real progress.
fn pending_grapheme_prefix_retries_the_same_line_candidate_after_real_progress() {
    let (_directory, source) = source("a b🙂c");
    let authority = source.authority();
    let mut job = MarkdownLayoutBreakJob::new(source, &authority).unwrap();
    // Complete only the actual line algorithm here, deliberately leaving the known opportunity's grapheme prefix unknown.
    while !job.payload.as_ref().unwrap().line.is_ready() {
        job.payload
            .as_mut()
            .unwrap()
            .line
            .step(&authority, MarkdownLineBreakBudget::default())
            .unwrap();
    }
    let mut cursor = job
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            0,
            MarkdownLayoutBreakQuery::Next,
        )
        .unwrap();
    assert_eq!(
        job.query(&mut cursor, 64).unwrap(),
        MarkdownLayoutBreakAnswer::PendingSource
    );
    let pending = cursor.candidate.unwrap();
    finish(&mut job, &authority);
    assert_eq!(
        job.query(&mut cursor, 64).unwrap(),
        MarkdownLayoutBreakAnswer::Known(Some(pending))
    );
}

#[test]
// Equal Source identity does not transfer cursor ownership between jobs; cancellation releases each real index without inventing shared-source release.
fn cursors_do_not_transfer_between_equal_sources_and_cancellation_releases_both_indexes() {
    let (_directory, source) = source("a🙂 b\r\n中文");
    let authority = source.authority();
    let mut first = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    let mut second = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    let mut cursor = first
        .cursor(
            MarkdownLayoutBreakUnit::Byte,
            0,
            MarkdownLayoutBreakQuery::Next,
        )
        .unwrap();
    assert!(matches!(
        second.query(&mut cursor, 8),
        Err(MarkdownLayoutBreakError::CursorMismatch)
    ));
    finish(&mut first, &authority);
    finish(&mut second, &authority);
    let observer = first.resource_observer();
    first.cancel();
    let (grapheme, line) = observer.snapshots();
    assert!(
        !grapheme.metadata_file_open
            && grapheme.metadata_map_bytes == 0
            && !grapheme.cleanup_failed
    );
    assert!(!line.metadata_file_open && line.metadata_map_bytes == 0 && !line.cleanup_failed);
    assert!(!grapheme.source_payload_released); // The second job/caller still legitimately owns this Source; release cannot be reported early.
    assert!(matches!(
        first.query(&mut cursor, 8),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
    second.cancel();
    drop(source);
    let (grapheme, line) = observer.snapshots();
    assert!(grapheme.source_payload_released && line.source_payload_released);
}

#[test]
// Empty EOF is an actual known boundary, while foreign capture authority poisons the original job rather than returning unknown/default content.
fn wrong_authority_poisoning_and_empty_eof_are_not_reported_as_unknown_or_plain_text() {
    let (_directory, source) = source("");
    let authority = source.authority();
    let mut empty = MarkdownLayoutBreakJob::new(source.clone(), &authority).unwrap();
    finish(&mut empty, &authority);
    let mut at = empty
        .cursor(
            MarkdownLayoutBreakUnit::Utf16,
            0,
            MarkdownLayoutBreakQuery::At,
        )
        .unwrap();
    assert!(
        matches!(empty.query(&mut at, 1).unwrap(), MarkdownLayoutBreakAnswer::Known(Some(boundary)) if boundary.utf16 == 0)
    );
    let (_other_directory, other) = self::source("");
    assert!(
        empty
            .step(&other.authority(), MarkdownLineBreakBudget::default())
            .is_err()
    );
    assert!(matches!(
        empty.cursor(
            MarkdownLayoutBreakUnit::Byte,
            0,
            MarkdownLayoutBreakQuery::At
        ),
        Err(MarkdownLayoutBreakError::Poisoned)
    ));
}
