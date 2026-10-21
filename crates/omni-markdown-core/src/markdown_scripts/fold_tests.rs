//! Differential coverage of real paragraph skips in the original SCX kernel.
//! A large hidden range checks bounded source reads and index resources, not RSS.

use super::{self as scripts, *};
use crate::markdown_layout_source::folds::FoldPlan;

/// Shares the original frozen source between normal and folded jobs. A fresh
/// capture with the same document generation would not authorize this proof.
fn folded(source: &MarkdownLayoutTextSource, hidden: std::ops::Range<usize>) -> MarkdownScriptJob {
    let authority = source.authority();
    let mut job = MarkdownScriptJob::new_streaming(
        source.clone(),
        &authority,
        MarkdownScriptCancellation::new(),
    )
    .unwrap();
    job.install_fold_plan(FoldPlan::new(source, std::slice::from_ref(&hidden)).unwrap())
        .unwrap();
    job
}

#[test]
// Complete visible runs must match original SCX. CRLF, brackets, and following
// non-Latin text must not inherit the skipped body's context.
fn folded_script_crlf_resets_original_bracket_context_and_preserves_visible_source_runs() {
    let tiny = MarkdownScriptBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        stack_ops: 1,
    };
    for ending in ["\n", "\r", "\r\n"] {
        let header = "# α(";
        let body = "中文(א)👩\u{200d}💻";
        let suffix = "Русский [abc]中文 e\u{301}";
        let text = format!("{header}{ending}{body}{ending}{suffix}");
        let (_directory, document) = scripts::fixture(&text);
        let source = MarkdownLayoutTextSource::capture(&document);
        let authority = source.authority();
        let start = header.encode_utf16().count();
        let end = start + ending.len() + body.encode_utf16().count() + ending.len();
        let mut original = MarkdownScriptJob::new_streaming(
            source.clone(),
            &authority,
            MarkdownScriptCancellation::new(),
        )
        .unwrap();
        let mut hidden = folded(&source, start..end);
        scripts::finish(&mut original, &authority, tiny);
        scripts::finish(&mut hidden, &authority, tiny);
        // Compare only visible original-source coordinates. The fold plan grants
        // no script answer inside the hidden range, even after both jobs finish.
        let mut utf16 = 0usize;
        for scalar in text.chars() {
            if utf16 < start || utf16 >= end {
                assert_eq!(
                    hidden
                        .run_at(&authority, MarkdownScriptUnit::Utf16, utf16)
                        .unwrap(),
                    original
                        .run_at(&authority, MarkdownScriptUnit::Utf16, utf16)
                        .unwrap()
                );
            }
            utf16 += scalar.len_utf16();
        }
        assert!(matches!(
            hidden.run_at(&authority, MarkdownScriptUnit::Utf16, start + 1),
            Err(MarkdownScriptError::FoldedRange)
        ));
        // A hidden query is a local refusal, not poison: the following visible
        // run remains usable through the same original authority.
        assert!(matches!(
            hidden
                .run_at(&authority, MarkdownScriptUnit::Utf16, end)
                .unwrap(),
            MarkdownScriptAnswer::Known(Some(_))
        ));
        assert_eq!(
            hidden.storage_profile().source_bytes_read,
            header.len() + ending.len() * 2 + suffix.len()
        );
        hidden.close().unwrap();
        original.close().unwrap();
    }
}

#[test]
// Hidden text must not scale real reads or indexes. Close relinquishes the job's
// actual FDs, mappings, and frozen-source claim.
fn folded_giant_script_body_does_not_read_or_spool_hidden_characters_and_releases_original_resources()
 {
    let header = "# H";
    let body = "中🙂[א]".repeat(131_072);
    let suffix = "after";
    let text = format!("{header}\r\n{body}\r\n{suffix}");
    let (_directory, document) = scripts::fixture(&text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = source.authority();
    let hidden_end = header.len() + 2 + body.encode_utf16().count() + 2;
    let mut job = folded(&source, header.len()..hidden_end);
    let observer = job.resource_observer();
    scripts::finish(&mut job, &authority, MarkdownScriptBudget::default());
    // Only the visible header/suffix and actual CRLF tails are borrowed. Hidden
    // brackets must create neither script records nor bracket-stack metadata.
    let actual = job.storage_profile();
    assert_eq!(actual.source_bytes_read, header.len() + 4 + suffix.len());
    assert!(actual.completed_runs <= 3 && actual.bracket_records == 0);
    assert!(matches!(
        job.run_at(&authority, MarkdownScriptUnit::Utf16, hidden_end)
            .unwrap(),
        MarkdownScriptAnswer::Known(Some(_))
    ));
    job.close().unwrap();
    let released = observer.snapshot();
    assert!(
        !released.run_file_open
            && !released.stack_file_open
            && released.mapped_bytes == 0
            && !released.cleanup_failed
    );
    drop(source);
    // The document remains live; this receipt specifically observes the frozen
    // layout source after its final external clone and the job have gone away.
    assert!(observer.snapshot().source_payload_released);
}
