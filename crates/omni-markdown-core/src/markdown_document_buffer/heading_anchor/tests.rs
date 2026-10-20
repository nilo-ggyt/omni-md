//! 合成真实文件经公开生产请求完成；不伪造 Fact、Source 身份、存储错误或私有阶段。

use super::{Error, MarkdownHeadingAnchorRequest, MarkdownHeadingAnchorStep};
use crate::{MarkdownDocumentBuffer, MarkdownTextSelection};
use std::{fs::File, io::Write, path::Path};

fn open(directory: &Path, name: &str, source: &str) -> MarkdownDocumentBuffer {
    let path = directory.join(name);
    std::fs::write(&path, source).unwrap();
    MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap()
}

/// 等原生产 continuation 的真实终点；工作量来自公开收据，不检查内部 phase 或调用次数。
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
        // 这是真实输入物化边界，不能靠超长标题或一次 fallback 绕开已有视口预算。
        assert!(work.source_bytes <= 4096);
        assert!(work.copied_bytes <= 4096);
        // 标题自身仍≤2KiB；真实引用值复用原生产定义解析器的16KiB有限值上限。
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
    let mut missing = document.heading_anchor_request("假标题").unwrap();
    assert_eq!(complete(&mut missing, &document).unwrap(), None);
    assert_eq!(
        std::fs::read(directory.path().join("headings.md")).unwrap(),
        source.as_bytes()
    );
}

#[test]
fn giant_plain_heading_and_following_heading_are_streamed_without_full_materialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("giant-heading.md");
    let mut file = File::create(&path).unwrap();
    file.write_all(b"# ").unwrap();
    for _ in 0..2048 {
        // 固定源块跨真实 mmap 页/UTF8边界；测试初始化同样不构造整条标题 String。
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
    // 已完成请求及目标只保 Weak；文档退出后真实 munmap 不得被导航缓存延长。
    drop(document);
    assert!(mapping.unmap_succeeded());
    assert!(!mapping.failed());
}

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

#[test]
fn giant_complex_heading_projects_real_markup_and_keeps_the_following_heading_reachable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("long-inline.md");
    let mut file = File::create(&path).unwrap();
    file.write_all("# 开头<!--".as_bytes()).unwrap();
    for _ in 0..1024 {
        // 超长注释是原HTML语法的一部分；两种请求都不能复制它形成标题String。
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
        // quoted属性里的'>'和']'不能提前关闭HTML或外层label；真实词法跨页继续。
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

#[test]
fn heading_reference_labels_use_real_later_definitions_and_keep_missing_references_literal() {
    let directory = tempfile::tempdir().unwrap();
    let source = "# [标签][定义]\n\n# [不存在][缺失]\n\n[定义]: https://example.invalid\n";
    let document = open(directory.path(), "reference.md", source);
    let mut resolved = document.heading_anchor_request("标签").unwrap();
    assert_eq!(complete(&mut resolved, &document).unwrap(), Some(0));
    // Missing 只有整个原 Global 已到EOF才能生效；不能把晚到定义当成不存在。
    let mut missing = document.heading_anchor_request("不存在缺失").unwrap();
    let expected = source[..source.find("# [不存在]").unwrap()]
        .encode_utf16()
        .count();
    assert_eq!(complete(&mut missing, &document).unwrap(), Some(expected));
}

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
