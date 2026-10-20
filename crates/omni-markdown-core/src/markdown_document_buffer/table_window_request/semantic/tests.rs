use super::*;
use crate::markdown_viewport_presentation::{
    MarkdownPresentationStyles, MarkdownViewportPresentation,
};

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("S2-semantic-fixture.md");
    std::fs::write(&path, source).unwrap();
    let doc = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, doc)
}
fn finish(
    request: &mut MarkdownTableWindowRequest,
    document: &MarkdownDocumentBuffer,
) -> MarkdownTableWindowStep {
    for _ in 0..document.byte_len().saturating_mul(64).saturating_add(10000) {
        let step = request.advance(document, false, 4096).unwrap();
        if !matches!(step, MarkdownTableWindowStep::Progress { .. }) {
            return step;
        }
    }
    panic!("semantic fixture exceeded finite driver allowance");
}
fn assert_matches_original(
    source: &str,
    probe: usize,
    viewport: Range<usize>,
    selection: Range<usize>,
) {
    let (_directory, doc) = document(source);
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(probe),
            viewport,
            93,
            selection.clone(),
        )
        .unwrap();
    assert!(matches!(
        finish(&mut request, &doc),
        MarkdownTableWindowStep::Ready { .. }
    ));
    assert!(request.lines.is_empty() && request.lines.capacity() == 0);
    assert!(
        request.queries.is_none()
            && request.pending_reference.is_none()
            && request.references.is_none()
            && request.reference_fact.is_none()
    );
    assert!(request.copier.is_none() && request.scope_cursor.is_none() && request.ticket.is_none());
    assert!(request.prefix_runs.is_empty() && request.prefix_runs.capacity() == 0);
    assert!(request.underline.is_none());
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    let window = request.ready_for(&doc).unwrap().unwrap();
    let presentation = window.presentation_for(&doc).unwrap();
    assert_eq!(presentation.context_revision, 93);
    let mut original = MarkdownViewportPresentation::default();
    original.set_active_selection(selection);
    original
        .rebuild(doc.generation(), 0..doc.utf16_len(), source)
        .unwrap();
    for coverage in window.coverage_for(&doc).unwrap() {
        for at in coverage.clone() {
            let style = |runs: &[MarkdownPresentationRun]| {
                runs.iter()
                    .find(|run| run.utf16_range.contains(&at))
                    .map_or(MarkdownPresentationStyles::default(), |run| run.styles)
            };
            assert_eq!(
                style(&presentation.runs),
                style(original.runs()),
                "at={at} source={source:?}"
            );
        }
    }
}

#[test]
fn complete_request_preserves_full_physical_container_prefix_and_unicode_styles() {
    // 整个物理源coverage逐UTF16对照，包含以前cell-only测试漏掉的H/D/body前缀与EOL。
    for source in [
        "| H | C |\n| - | - |\n| **一** | $x$ |\n| [r] | ~~删~~ |\n\n[r]: /first \"title\"\n[r]: /later\n",
        "> > | H | C |\r\n> > | - | - |\r\n> > | **一** | $x$ |\r\n> > | [r] | ~~删~~ |\r\n\r\n[r]: /r\r\n",
        "-   item\n\n    > | H | C |\n    > | - | - |\n    > | **一** | $x$ |\n    > | [r] | ~~删~~ |\n\n[r]: /r\n",
        "[^n]:\n    | H | C |\n    | - | - |\n    | **一** | $x$ |\n    | [r] | ~~删~~ |\n\n[r]: /r\n\n[^n]\n",
        "3. | H | C |\n   | - | - |\n   | **一** | $x$ |\n   | [r] | ~~删~~ |\n\n[r]: /r\n",
    ] {
        let probe_byte = source.find("**一**").unwrap();
        let probe = source[..probe_byte].encode_utf16().count();
        assert_matches_original(
            source,
            probe,
            0..source.encode_utf16().count(),
            usize::MAX..usize::MAX,
        );
        // 省略H时仍保body真实容器属性；中段窗不能凭H虚拟拼接给省略gap任何覆盖权限。
        let body_start = source[..probe_byte].rfind('\n').map_or(0, |at| at + 1);
        let start = source[..body_start].encode_utf16().count();
        assert_matches_original(
            source,
            probe,
            start..source.encode_utf16().count(),
            probe..probe,
        );
    }
}

#[test]
fn global_reference_underline_and_math_context_are_all_required_before_ready() {
    let hidden = format!("$x${}", "{}".repeat(255));
    let source = format!(
        "<u>opening\n\n| H | C |\n| - | - |\n| {hidden} | skipped |\n| $}}x{{y$ [r] | 中文😀 |\n\n</u>\n\n[r]: /real \"title\"\n"
    );
    let probe_byte = source.find("$}x{y$").unwrap();
    let probe = source[..probe_byte].encode_utf16().count();
    let start_byte = source[..probe_byte].rfind('\n').unwrap() + 1;
    let end_byte = source[probe_byte..].find('\n').unwrap() + probe_byte + 1;
    let viewport =
        source[..start_byte].encode_utf16().count()..source[..end_byte].encode_utf16().count();
    assert_matches_original(&source, probe, viewport, usize::MAX..usize::MAX);
}

#[test]
fn standalone_table_underline_uses_literal_code_facts_before_and_after_its_viewport() {
    for source in [
        "<u>外部开标记\n\n```text\n</u>围栏字面\n```\n\n| H | C |\n| - | - |\n| [r] | 中文😀 |\n\n    </u><u>缩进字面\n\n外部真实关闭</u>\n\n[r]: /first\n[r]: /later\n",
        "<u>外部开标记\r\n\r\n>\t```text\r\n>\t</u>围栏字面\r\n>\t```\r\n>\r\n> | H | C |\r\n> | - | - |\r\n> | [r] | 中文😀 |\r\n\r\n    </u><u>缩进字面\r\n\r\n外部真实关闭</u>\r\n\r\n[r]: /first\r\n[r]: /later\r\n",
    ] {
        let probe_byte = source.find("[r]").unwrap();
        let probe = source[..probe_byte].encode_utf16().count();
        let start = source[..probe_byte].rfind('\n').unwrap() + 1;
        let end = probe_byte + source[probe_byte..].find('\n').unwrap() + 1;
        // standalone的原结构Global要存活到配对恢复完成；不能丢它后再创建另一个分类器。
        assert_matches_original(
            source,
            probe,
            source[..start].encode_utf16().count()..source[..end].encode_utf16().count(),
            usize::MAX..usize::MAX,
        );
    }
}

#[test]
fn verified_window_cannot_authorize_other_note_or_stale_edit_and_releases_scratch() {
    let source = "| H | C |\n| - | - |\n| first | second |\n";
    let (_directory, mut doc) = document(source);
    let (_other_directory, other) = document(source);
    let probe = source.find("first").unwrap();
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(probe),
            0..doc.utf16_len(),
            4,
            usize::MAX..usize::MAX,
        )
        .unwrap();
    finish(&mut request, &doc);
    let window = request.take_ready_for(&doc).unwrap().unwrap();
    assert!(request.ready_for(&doc).unwrap().is_none());
    assert_eq!(
        window.presentation_for(&other).unwrap_err(),
        MarkdownTableRequestError::SourceSuperseded
    );
    let plan = doc
        .plan_verified_table_cell_edit(&window, 1, 0, "updated")
        .unwrap();
    let (range, replacement) = plan.replacement_for(&doc).unwrap();
    doc.replace_utf16_range(range.clone(), replacement, MarkdownTextSelection::caret(0))
        .unwrap();
    assert!(window.presentation_for(&doc).is_err());
    doc.undo();
    assert!(window.presentation_for(&doc).is_err());
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    assert!(request.lines.is_empty() && request.lines.capacity() == 0);
}

#[test]
fn consumed_request_cannot_republish_an_empty_ready_slot() {
    // 同代次也只能取走一次；空请求不能继续回报Ready并让Native误认为已有完整窗口。
    let (_directory, doc) = document("| H |\n| - |\n| value |\n");
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(2),
            0..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    finish(&mut request, &doc);
    assert!(request.take_ready_for(&doc).unwrap().is_some());
    assert_eq!(
        request.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::InvalidRequest)
    );
    assert!(request.take_ready_for(&doc).is_err());
}

#[test]
fn offscreen_header_retains_cell_styles_without_becoming_a_visible_row_or_gap() {
    // 中段窗口仍用H测列宽，但H到body的省略区间没有属性/geometry覆盖权限。
    let source = "<u>before\n\n> | **中文** [r] | $x$ 😀 |\n> | - | - |\n> | skipped | old |\n> | visible | cell |\n\n</u>\n\n[r]: /real\n";
    let (_directory, doc) = document(source);
    let start = source.find("> | visible").unwrap();
    let end = start + source[start..].find('\n').unwrap() + 1;
    let units = source[..start].encode_utf16().count()..source[..end].encode_utf16().count();
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(units.start),
            units.clone(),
            7,
            usize::MAX..usize::MAX,
        )
        .unwrap();
    finish(&mut request, &doc);
    let ready = request.ready_for(&doc).unwrap().unwrap();
    let actual = ready.presentation_for(&doc).unwrap();
    assert_eq!(actual.rows.len(), 1);
    assert_eq!(actual.rows[0].row_ordinal, 2);
    assert_eq!(ready.coverage_for(&doc).unwrap(), &[units]);
    assert_eq!(actual.header_cells().len(), 2);
    assert!(actual.rows.iter().all(|row| !row.is_header));
    let mut original = MarkdownViewportPresentation::default();
    original.set_active_selection(usize::MAX..usize::MAX);
    original
        .rebuild(doc.generation(), 0..doc.utf16_len(), source)
        .unwrap();
    // 仅对H格真实源范围逐unit比较；H容器marker无需进入当前绘制矩阵。
    for cell in actual.header_cells() {
        if let crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource::Present(range) = &cell.source {
            for at in range.clone() {
                let style = |runs: &[MarkdownPresentationRun]| runs.iter().find(|run| run.utf16_range.contains(&at)).map_or(MarkdownPresentationStyles::default(), |run| run.styles);
                assert_eq!(style(&actual.header_context.runs), style(original.runs()), "header unit={at}");
            }
        }
    }
    assert!(
        actual
            .runs
            .iter()
            .all(|run| run.utf16_range.start >= actual.rows[0].utf16_range.start)
    );
}

#[test]
fn measured_validation_includes_revisited_prefix_and_scope_metadata_payload() {
    let source = format!("| {}中 |\n| - |\n| value |\n", "a".repeat(996));
    let (_directory, doc) = document(&source);
    let mut request = doc
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(0),
            0..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    let mut reads = 0;
    let mut validation = 0;
    loop {
        let copying = matches!(request.phase, Phase::Copy);
        let step = request.advance(&doc, false, 4096).unwrap();
        if copying && let MarkdownTableWindowStep::Progress { work } = step {
            reads += work.source_bytes;
            validation += work.utf8_validation_bytes;
            // 只对真正逐byte的复制阶段求和；有限Parser/marker helper输入另列，不能混称逐tokenfuel。
            assert!(
                work.source_bytes
                    + work.copied_source_bytes
                    + work.utf8_validation_bytes
                    + work.source_tree_steps
                    + work.structural_transitions
                    <= 4096
            );
        }
        if !matches!(step, MarkdownTableWindowStep::Progress { .. }) {
            break;
        }
    }
    assert!(
        validation > reads,
        "frame尾scalar使valid prefix实际被再次校验，必须如实计量"
    );

    let prefix = "> ".repeat(16);
    let source = format!("{prefix}| H |\n{prefix}| - |\n{prefix}| value |\n");
    let (_directory, doc) = document(&source);
    let body_start = source.rfind(&format!("{prefix}| value")).unwrap();
    for (viewport, expected_output) in [
        (0..doc.utf16_len(), true),
        (body_start..doc.utf16_len(), false),
    ] {
        let mut request = doc
            .table_window_request(
                MarkdownTableWindowTarget::AtUtf16(viewport.start),
                viewport,
                0,
                0..0,
            )
            .unwrap();
        let mut metadata = 0;
        loop {
            let step = request.advance(&doc, false, 4096).unwrap();
            let work = match step {
                MarkdownTableWindowStep::Progress { work }
                | MarkdownTableWindowStep::Ready { work } => work,
                _ => panic!("quote fixture lost its table"),
            };
            metadata += work.metadata_bytes;
            if matches!(step, MarkdownTableWindowStep::Ready { .. }) {
                break;
            }
        }
        // 只有H可见才准入其真实marker输出；离屏H只保固定kind状态，不能虚计Vec payload。
        assert_eq!(metadata > 0, expected_output);
    }
}

#[test]
fn viewport_target_finds_real_tables_below_plain_text_and_continues_after_true_end() {
    let source = "intro\n\n```text\n| fake |\n| - |\n```\n\n| first |\n| - |\n| body |\n\nordinary\n\n| second |\n| - |\n| tail |\n";
    let (_directory, doc) = document(source);
    let mut first = doc
        .table_window_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            0..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    assert!(matches!(
        finish(&mut first, &doc),
        MarkdownTableWindowStep::Ready { .. }
    ));
    let window = first.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(
        window
            .presentation_for(&doc)
            .unwrap()
            .header_byte_range
            .start,
        source.find("| first").unwrap()
    );
    let after_first = window.table_end_for(&doc).unwrap().unwrap().utf16_end();
    let mut second = doc
        .table_window_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            after_first..doc.utf16_len(),
            0,
            0..0,
        )
        .unwrap();
    finish(&mut second, &doc);
    let window = second.take_ready_for(&doc).unwrap().unwrap();
    assert_eq!(
        window
            .presentation_for(&doc)
            .unwrap()
            .header_byte_range
            .start,
        source.find("| second").unwrap()
    );
    // 有限前缀没有表的证明与整个文档无表不同；未来真实表不进入本批投影。
    let mut prefix = doc
        .table_window_request(
            MarkdownTableWindowTarget::FirstTableInViewport,
            0..5,
            0,
            0..0,
        )
        .unwrap();
    assert!(matches!(
        finish(&mut prefix, &doc),
        MarkdownTableWindowStep::NotTable { .. }
    ));
}

#[test]
/// S2逐层quote使用真实H锚点与原物理行端点，不把footnote label里的`>`当引用前缀。
fn quote_prefix_owners_match_full_parser_for_endpoint_lines_and_footnote_labels() {
    for source in [
        "> > | H | C |\r\n> > | - | - |\r\n> > | 中文😀 | x |\r\n> > | 尾部 | y |",
        "[^a>]: > | H | C |\n    > | - | - |\n    > | 中文😀 | x |\n    > | 尾部 | y |\n\n[^a>]\n",
        "> [^a>]: > | H | C |\n>     > | - | - |\n>     > | 中文😀 | x |\n>     > | 尾部 | y |\n\n[^a>]\n",
    ] {
        let body_byte = source.find("中文").unwrap();
        let probe = source[..body_byte].encode_utf16().count();
        let line_byte = source[..body_byte].rfind('\n').map_or(0, |at| at + 1);
        let line_start = source[..line_byte].encode_utf16().count();
        let length = source.encode_utf16().count();
        for selection in [
            usize::MAX..usize::MAX,
            line_start..line_start,
            length..probe,
        ] {
            // 保留隐藏位和每个全局UTF16位置的精确oracle；中段gap仍不授予不存在的H覆盖。
            assert_matches_original(source, probe, 0..length, selection.clone());
            assert_matches_original(source, probe, line_start..length, selection);
        }
    }
}
