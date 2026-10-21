//! Synthetic real files exercise public production requests without fabricated
//! facts, source identities, storage errors, or private phase mutation.

use super::{Error, MarkdownHeadingAnchorRequest, MarkdownHeadingAnchorStep};
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};
use std::{fs::File, io::Write, path::Path};

/// Opens a real, caller-owned synthetic file through the production immutable
/// snapshot path; no user vault or fabricated source authority participates.
fn open(directory: &Path, name: &str, source: &str) -> MarkdownDocumentBuffer {
    let path = directory.join(name);
    std::fs::write(&path, source).unwrap();
    MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap()
}

/// Advances the original production continuation to its real endpoint. Work
/// comes from public receipts, never private phases or expected call counts.
fn complete(
    request: &mut MarkdownHeadingAnchorRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<Option<usize>, Error> {
    for _ in 0..1_000_000 {
        let step = request.advance(document, 4096)?;
        let work = match &step {
            MarkdownHeadingAnchorStep::Progress { work }
            | MarkdownHeadingAnchorStep::Found { work, .. }
            | MarkdownHeadingAnchorStep::NotFound { work } => work,
        };
        // These are actual input-materialization boundaries. A giant heading or
        // fallback must not bypass the existing viewport read/copy budgets.
        assert!(work.source_bytes <= 4096);
        assert!(work.copied_bytes <= 4096);
        // Local heading parser inputs remain bounded; actual reference values
        // retain the production definition parser's separate 16-KiB value limit.
        assert!(work.parser_input_bytes <= 16 * 1024);
        assert!(work.storage_bytes <= 4096);
        match step {
            MarkdownHeadingAnchorStep::Found { target, .. } => {
                return target.position_for(document).map(Some);
            }
            MarkdownHeadingAnchorStep::NotFound { .. } => return Ok(None),
            MarkdownHeadingAnchorStep::Progress { .. } => {}
        }
    }
    panic!("标题链接没有在有限合成源上完成");
}

/// Visible inline content, repeated slug collisions, quotes, lists, and Setext
/// headings must navigate to their actual original physical lines without editing disk.
#[test]
fn real_markdown_heading_text_and_duplicate_collisions_navigate_to_original_lines() {
    let directory = tempfile::tempdir().unwrap();
    let source = concat!(
        "```md\n# 假标题\n```\n\n",
        "# 标题 **粗体** *斜体* `a_b` [链接](https://example.invalid) &amp; 中文\n\n",
        "# A\n\n# A-1\n\n# A\n\n# A\n\n",
        "> ## 引用标题\n\n",
        "- 第一层\n  - 第二层\n\n    ## 深层标题\n\n",
        "Setext 中文\n==========\n\n",
        "> Scoped Setext\n> =============\n",
    );
    let document = open(directory.path(), "headings.md", source);
    for (fragment, original_line) in [
        ("标题-粗体-斜体-a_b-链接--中文", "# 标题"),
        ("a", "# A\n"),
        ("a-1", "# A-1"),
        ("a-2", "# A\n\n# A\n"),
        ("a-3", "# A\n\n>"),
        ("引用标题", "> ## 引用标题"),
        ("深层标题", "    ## 深层标题"),
        ("setext-中文", "Setext 中文"),
        ("scoped-setext", "> Scoped Setext"),
    ] {
        let expected = source[..source.find(original_line).unwrap()]
            .encode_utf16()
            .count();
        let mut request = document.heading_anchor_request(fragment).unwrap();
        assert_eq!(
            complete(&mut request, &document).unwrap(),
            Some(expected),
            "{fragment}"
        );
    }
    // A heading-looking line inside a real code fence never acquires anchor semantics.
    let mut missing = document.heading_anchor_request("假标题").unwrap();
    assert_eq!(complete(&mut missing, &document).unwrap(), None);
    assert_eq!(
        std::fs::read(directory.path().join("headings.md")).unwrap(),
        source.as_bytes()
    );
}

/// A giant irrelevant heading must not block a later actual heading or retain
/// a document-sized read window after navigation completes.
#[test]
fn giant_plain_heading_and_following_heading_are_streamed_without_full_materialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("giant-heading.md");
    let mut file = File::create(&path).unwrap();
    file.write_all(b"# ").unwrap();
    for _ in 0..2048 {
        // Fixed source chunks cross real mmap and UTF-8 boundaries. Even fixture
        // setup avoids constructing the entire heading as one string.
        file.write_all("中文".repeat(32).as_bytes()).unwrap();
    }
    file.write_all("\n\n# 后继标题\n".as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let original = std::fs::metadata(&path).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let mut request = document.heading_anchor_request("后继标题").unwrap();
    assert_eq!(
        complete(&mut request, &document).unwrap(),
        Some(2 + 2048 * 64 + 2)
    );
    assert!(mapping.peak_managed_resident_bytes() <= 64 * 1024);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), original.len());
    // Completed requests and targets retain weak identity only. After document
    // destruction, navigation metadata must not delay the actual unmap.
    drop(document);
    assert!(mapping.unmap_succeeded());
    assert!(!mapping.failed());
}

/// Equal content and undo-restored bytes do not recreate the original request's
/// document/generation authority; stale navigation must reject rather than retarget.
#[test]
fn requests_and_results_cannot_survive_edit_undo_or_an_equal_length_different_document() {
    let directory = tempfile::tempdir().unwrap();
    let mut document = open(directory.path(), "first.md", "# 原标题\n");
    let other = open(directory.path(), "other.md", "# 原标题\n");
    let mut wrong_document = document.heading_anchor_request("原标题").unwrap();
    assert_eq!(
        wrong_document.advance(&other, 4096).unwrap_err(),
        Error::SourceSuperseded
    );
    let mut request = document.heading_anchor_request("原标题").unwrap();
    let target = loop {
        match request.advance(&document, 4096).unwrap() {
            MarkdownHeadingAnchorStep::Found { target, .. } => break target,
            MarkdownHeadingAnchorStep::Progress { .. } => {}
            MarkdownHeadingAnchorStep::NotFound { .. } => panic!("真实标题必须找到"),
        }
    };
    document
        .replace_utf16_range(0..0, "前缀\n", MarkdownTextSelection::caret(3))
        .unwrap();
    assert_eq!(
        target.position_for(&document).unwrap_err(),
        Error::SourceSuperseded
    );
    // Undo can restore identical source bytes but not the consumed generation.
    // Both the finished target and original continuation must remain revoked.
    document.undo().unwrap();
    assert_eq!(
        target.position_for(&document).unwrap_err(),
        Error::SourceSuperseded
    );
    assert_eq!(
        request.advance(&document, 4096).unwrap_err(),
        Error::SourceSuperseded
    );
}

/// Cancellation is an explicit failure, never a successful NotFound. Abandoned
/// navigation keeps no strong source owner that could delay actual document unmap.
#[test]
fn cancellation_and_abandonment_release_original_mapping_without_claiming_a_missing_anchor() {
    let directory = tempfile::tempdir().unwrap();
    let document = open(directory.path(), "cancel.md", "# 第一节\n\n# 第二节\n");
    let mapping = document.original_backing_observer().unwrap();
    let mut request = document.heading_anchor_request("不存在").unwrap();
    assert!(matches!(
        request.advance(&document, 4096).unwrap(),
        MarkdownHeadingAnchorStep::Progress { .. }
    ));
    request.cancel().unwrap();
    for _ in 0..2 {
        assert_eq!(
            request.advance(&document, 4096).unwrap_err(),
            Error::Cancelled
        );
    }
    let abandoned = document.heading_anchor_request("第一节").unwrap();
    drop(document);
    assert!(mapping.unmap_succeeded());
    drop(abandoned);
}

/// Empty documents still have distinct owners even though both roots and source
/// contents are empty. A zero-position target cannot cross that ownership boundary.
#[test]
fn an_empty_document_anchor_keeps_the_original_empty_owner_identity() {
    let directory = tempfile::tempdir().unwrap();
    let document = open(directory.path(), "empty-first.md", "");
    let other = open(directory.path(), "empty-other.md", "");
    let mut request = document.heading_anchor_request("").unwrap();
    let target = match request.advance(&document, 4096).unwrap() {
        MarkdownHeadingAnchorStep::Found { target, .. } => target,
        _ => panic!("空片段必须到同一文档首部"),
    };
    assert_eq!(target.position_for(&document).unwrap(), 0);
    assert_eq!(
        target.position_for(&other).unwrap_err(),
        Error::SourceSuperseded
    );
}

/// Giant hidden HTML and real surrounding inline markup must project the exact
/// visible slug while preserving reachability and bounded residency for a later heading.
#[test]
fn giant_complex_heading_projects_real_markup_and_keeps_the_following_heading_reachable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("long-inline.md");
    let mut file = File::create(&path).unwrap();
    file.write_all("# 开头<!--".as_bytes()).unwrap();
    for _ in 0..1024 {
        // The giant comment belongs to actual HTML syntax. Neither request may
        // copy its contents into a complete heading string.
        file.write_all(&[b'x'; 1024]).unwrap();
    }
    file.write_all(
        concat!(
            "-->**粗体** *斜体* `a_b &amp;` [链接](https://example.invalid) &amp; 中文\n\n",
            "# 后继标题\n",
        )
        .as_bytes(),
    )
    .unwrap();
    file.sync_all().unwrap();
    drop(file);
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    let mapping = document.original_backing_observer().unwrap();
    let mut exact = document
        .heading_anchor_request("开头粗体-斜体-a_b-amp-链接--中文")
        .unwrap();
    assert_eq!(complete(&mut exact, &document).unwrap(), Some(0));
    let mut following = document.heading_anchor_request("后继标题").unwrap();
    let tail = "-->**粗体** *斜体* `a_b &amp;` [链接](https://example.invalid) &amp; 中文\n\n";
    let expected = "# 开头<!--".encode_utf16().count() + 1024 * 1024 + tail.encode_utf16().count();
    assert_eq!(complete(&mut following, &document).unwrap(), Some(expected));
    assert!(mapping.peak_managed_resident_bytes() <= 64 * 1024);
    drop(document);
    assert!(mapping.unmap_succeeded());
    assert!(!mapping.failed());
}

/// Cross-page quoted HTML, nested labels, multiline Setext scope, CR/CRLF, and
/// later reference definitions must keep their original complete syntax decisions.
#[test]
fn long_setext_and_nested_link_labels_use_original_scope_and_real_later_references() {
    let directory = tempfile::tempdir().unwrap();
    let cases = [
        (
            "# 开头<span title=\"",
            "\">**粗体**</span> [![图](image)](target) <https://example.invalid>\n",
            "开头粗体-图-httpsexampleinvalid",
        ),
        (
            "> 开头<span title=\"",
            "\">**粗体**</span>  \r\n> *斜体* `甲\r\n> 乙` [标\r\n> 签][定\r\n> 义] &amp;\r\n> =========\r\n\r\n[定 义]: https://example.invalid\r\n",
            "开头粗体-斜体-甲-乙-标-签-",
        ),
        (
            "开头<span title=\"",
            "\">**粗体**</span>\r*斜体* `甲\r乙` &amp;\r=========\r",
            "开头粗体-斜体-甲-乙-",
        ),
        (
            "# 开头<span title=\"",
            "\">**粗体**</span> [未定义][缺失] &lt;无效 `代码`\n",
            "开头粗体-未定义缺失-无效-代码",
        ),
    ];
    for (index, (opening, ending, fragment)) in cases.into_iter().enumerate() {
        let path = directory.path().join(format!("long-scope-{index}.md"));
        let mut file = File::create(&path).unwrap();
        file.write_all(opening.as_bytes()).unwrap();
        // A quoted attribute's '>' or ']' cannot close HTML or the outer label
        // early; actual lexical state must continue across source pages.
        for _ in 0..8 {
            file.write_all(&[b'x'; 1024]).unwrap();
        }
        file.write_all(ending.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        let mut request = document.heading_anchor_request(fragment).unwrap();
        assert_eq!(
            complete(&mut request, &document).unwrap(),
            Some(0),
            "case={index}"
        );
    }
}

/// Resolved links contribute their visible label; only real whole-source EOF
/// permits an unresolved reference's literal spelling to remain in the anchor.
#[test]
fn heading_reference_labels_use_real_later_definitions_and_keep_missing_references_literal() {
    let directory = tempfile::tempdir().unwrap();
    let source = "# [标签][定义]\n\n# [不存在][缺失]\n\n[定义]: https://example.invalid\n";
    let document = open(directory.path(), "reference.md", source);
    let mut resolved = document.heading_anchor_request("标签").unwrap();
    assert_eq!(complete(&mut resolved, &document).unwrap(), Some(0));
    // Only the original whole-source frontier at real EOF proves missing.
    // A definition appearing later in the source must remain visible.
    let mut missing = document.heading_anchor_request("不存在缺失").unwrap();
    let expected = source[..source.find("# [不存在]").unwrap()]
        .encode_utf16()
        .count();
    assert_eq!(complete(&mut missing, &document).unwrap(), Some(expected));
}

/// Greek final sigma depends on actual visible surrounding letters across
/// markup and page boundaries; hidden HTML cannot create an artificial word edge.
#[test]
fn unicode_final_sigma_keeps_visible_context_across_markup_and_real_source_pages() {
    let directory = tempfile::tempdir().unwrap();
    let source = "# Ο**Σ**\n\n# ΟΣ.Α\n\n# Ο`Σ`\n\n# Σ\n";
    let document = open(directory.path(), "sigma.md", source);
    for (fragment, line) in [
        ("ος", "# Ο**Σ**"),
        ("οσα", "# ΟΣ.Α"),
        ("ος-1", "# Ο`Σ`"),
        ("σ", "# Σ"),
    ] {
        let mut request = document.heading_anchor_request(fragment).unwrap();
        let expected = source[..source.find(line).unwrap()].encode_utf16().count();
        assert_eq!(complete(&mut request, &document).unwrap(), Some(expected));
    }
    // Place the deciding visible suffix beyond a real multi-page hidden span.
    // This contrasts a final sigma with one followed by another cased letter.
    for (index, (ending, fragment)) in [("-->**Σ**\n", "ος"), ("-->**Σ**Α\n", "οσα")]
        .into_iter()
        .enumerate()
    {
        let path = directory.path().join(format!("sigma-long-{index}.md"));
        let mut file = File::create(&path).unwrap();
        file.write_all("# Ο<!--".as_bytes()).unwrap();
        for _ in 0..8 {
            file.write_all(&[b'x'; 1024]).unwrap();
        }
        file.write_all(ending.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        let mut request = document.heading_anchor_request(fragment).unwrap();
        assert_eq!(complete(&mut request, &document).unwrap(), Some(0));
    }
}

/// A later empty footnote definition still proves existence. Nested labels and
/// absent keys must preserve the same visible result in both short and giant headings.
#[test]
fn real_global_footnotes_include_later_empty_definitions_and_keep_missing_keys_literal() {
    let directory = tempfile::tempdir().unwrap();
    let source = "# 标题[^定义]\n\n# 缺失[^不存在]\n\n[^定义]:\n";
    let document = open(directory.path(), "footnotes.md", source);
    let mut found = document.heading_anchor_request("标题").unwrap();
    assert_eq!(complete(&mut found, &document).unwrap(), Some(0));
    let mut missing = document.heading_anchor_request("缺失不存在").unwrap();
    let expected = source[..source.find("# 缺失").unwrap()]
        .encode_utf16()
        .count();
    assert_eq!(complete(&mut missing, &document).unwrap(), Some(expected));
    for (index, (ending, fragment)) in [
        ("\">甲[^定义]乙</span>\n\n[^定义]:\n", "开头甲乙"),
        (
            "\">![图[^定义]片](target)</span>\n\n[^定义]: 定义内容\n",
            "开头图片target",
        ),
        ("\">甲[^不存在]乙</span>\n", "开头甲不存在乙"),
        (
            "\">[标签][^\n\n[标签]: https://example.invalid\n",
            "开头标签",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("footnote-long-{index}.md"));
        let mut file = File::create(&path).unwrap();
        file.write_all("# 开头<span title=\"".as_bytes()).unwrap();
        for _ in 0..8 {
            file.write_all(&[b'x'; 1024]).unwrap();
        }
        file.write_all(ending.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
        let mut request = document.heading_anchor_request(fragment).unwrap();
        assert_eq!(
            complete(&mut request, &document).unwrap(),
            Some(0),
            "case={index}"
        );
    }
}
