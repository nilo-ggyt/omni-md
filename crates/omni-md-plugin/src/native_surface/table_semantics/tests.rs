use super::*;
use omni_markdown_core::MarkdownTextSelection;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownTableWindowStep, MarkdownTableWindowTarget,
};

/// fixture 经真实 snapshot/piece tree 打开；测试不伪造 VerifiedWindow 的私有源权限。
fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("table-semantic-view.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}

/// 只有真实 producer 的 Ready 可交给 view；有限测试 driver 不进入产品调度路径。
fn ready(
    document: &MarkdownDocumentBuffer,
    target: usize,
    viewport: Range<usize>,
) -> VerifiedMarkdownTableWindow {
    let mut request = document
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(target),
            viewport,
            7,
            usize::MAX..usize::MAX,
        )
        .unwrap();
    let allowance = document
        .byte_len()
        .saturating_mul(64)
        .saturating_add(10_000);
    for _ in 0..allowance {
        // 每步沿同一原文档和固定预算推进；非 Ready 终点不能被当作可消费元数据。
        match request.advance(document, false, 4096).unwrap() {
            MarkdownTableWindowStep::Progress { .. } => {}
            MarkdownTableWindowStep::Ready { .. } => {
                return request.take_ready_for(document).unwrap().unwrap();
            }
            MarkdownTableWindowStep::NotTable { .. }
            | MarkdownTableWindowStep::TableEndReached { .. } => {
                panic!("真实表格 fixture 未产生 Ready");
            }
        }
    }
    panic!("真实表格 fixture 超过有限测试 driver 预算");
}

#[test]
fn actual_ready_keeps_adjacent_coverage_separate_and_preserves_cell_source_kind() {
    // 字面量坐标独立固定：H 0..13，D 14..27，body 28..39，末行 plain 40..45。
    let source = "| A | B | C |\n| - | - | - |\n| a | | c |\nplain";
    let (_directory, document) = document(source);
    let window = ready(&document, 0, 0..45);
    let view = NativeMarkdownTableSemanticView::new(&window, &document).unwrap();
    assert_eq!(view.coverage(), &[0..28, 28..45]);
    assert!(std::ptr::eq(
        view.presentation(),
        window.presentation_for(&document).unwrap(),
    ));
    // 两段虽相邻仍不合并：EOL 和下一源行的属性结果各自留在其获准段内。
    assert!(view.query_published(27).unwrap().utf16_range.end <= 28);
    assert!(view.query_published(28).unwrap().utf16_range.start >= 28);
    assert!(view.query_published(45).is_none());
    let explicit = view
        .cell(
            NativeMarkdownTableCellScope::PublishedRow { row_ordinal: 1 },
            1,
        )
        .unwrap();
    assert_eq!(
        explicit.source(),
        &MarkdownTableWindowCellSource::Present(34..34),
    );
    assert_eq!(explicit.row().row_ordinal, 1);
    assert_eq!(explicit.metadata().column_index, 1);
    assert!(explicit.query(34).is_none());
    let implicit = view
        .cell(
            NativeMarkdownTableCellScope::PublishedRow { row_ordinal: 2 },
            1,
        )
        .unwrap();
    assert_eq!(
        implicit.source(),
        &MarkdownTableWindowCellSource::ImplicitEmpty {
            insertion_anchor_utf16: 45,
        },
    );
    assert!(implicit.query(45).is_none());
    assert!(
        view.cell(
            NativeMarkdownTableCellScope::PublishedRow { row_ordinal: 2 },
            3,
        )
        .is_none()
    );
    // D 由同一真实 Ready 完整证明；header context 在 H 已发布时直接借主 cell。
    assert!(view.delimiter_is_hidden());
    let published_header = view
        .cell(
            NativeMarkdownTableCellScope::PublishedRow { row_ordinal: 0 },
            0,
        )
        .unwrap();
    let context_header = view
        .cell(NativeMarkdownTableCellScope::HeaderContext, 0)
        .unwrap();
    assert!(std::ptr::eq(
        published_header.metadata(),
        context_header.metadata(),
    ));
    assert!(
        context_header
            .query(2)
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::TABLE_HEADER)
    );
}

#[test]
fn offscreen_header_styles_do_not_publish_header_delimiter_or_omitted_row() {
    // 此真实 fixture 同时经过 quote、引用、公式、Unicode 和跨行 underline 上下文。
    let source = "<u>before\n\n> | **中文** [r] | $x$ 😀 |\n> | - | - |\n> | skipped | old |\n> | visible | cell |\n\n</u>\n\n[r]: /real\n";
    let (_directory, document) = document(source);
    let start = source.find("> | visible").unwrap();
    let end = start + source[start..].find('\n').unwrap() + 1;
    let viewport = source[..start].encode_utf16().count()..source[..end].encode_utf16().count();
    let window = ready(&document, viewport.start, viewport.clone());
    let view = NativeMarkdownTableSemanticView::new(&window, &document).unwrap();
    assert_eq!(view.coverage(), std::slice::from_ref(&viewport));
    assert_eq!(view.presentation().rows.len(), 1);
    assert_eq!(view.presentation().rows[0].row_ordinal, 2);
    assert!(view.query_published(0).is_none());
    assert!(view.query_published(viewport.start - 1).is_none());
    assert!(
        view.query_published(view.presentation().header_utf16_range.start)
            .is_none()
    );
    assert!(
        view.query_published(view.presentation().delimiter_utf16_range.start)
            .is_none()
    );
    assert!(!view.delimiter_is_hidden());
    assert!(
        view.cell(
            NativeMarkdownTableCellScope::PublishedRow { row_ordinal: 0 },
            0,
        )
        .is_none()
    );
    let body = view
        .cell(
            NativeMarkdownTableCellScope::PublishedRow { row_ordinal: 2 },
            0,
        )
        .unwrap();
    assert_eq!(body.row().row_ordinal, 2);
    let header = view
        .cell(NativeMarkdownTableCellScope::HeaderContext, 0)
        .unwrap();
    let MarkdownTableWindowCellSource::Present(range) = header.source() else {
        panic!("字面 H 必须提供真实 cell 源范围");
    };
    // header 样式可供单格测量，但同一位置的正文查询仍无权限。
    let query = header.query(range.start).unwrap();
    assert!(
        query
            .styles
            .contains(MarkdownPresentationStyles::TABLE_HEADER)
    );
    assert!(range.start <= query.utf16_range.start && query.utf16_range.end <= range.end);
    assert!(view.query_published(range.start).is_none());
}

#[test]
fn view_constructor_rejects_other_document_and_superseded_root() {
    let source = "| h |\n| - |\n| value |\n";
    let (_directory, mut first) = document(source);
    let (_other_directory, other) = document(source);
    let window = ready(&first, 0, 0..first.utf16_len());
    // 相同字节、长度和 generation 不代表同一个 snapshot/piece root。
    assert!(NativeMarkdownTableSemanticView::new(&window, &other).is_err());
    first
        .replace_utf16_range(2..3, "H", MarkdownTextSelection::caret(3))
        .unwrap();
    assert!(NativeMarkdownTableSemanticView::new(&window, &first).is_err());
}

#[test]
fn style_query_clips_styled_runs_and_default_gaps_to_exact_permission() {
    let runs = [
        MarkdownPresentationRun {
            utf16_range: 0..4,
            styles: MarkdownPresentationStyles::STRONG,
        },
        MarkdownPresentationRun {
            utf16_range: 9..16,
            styles: MarkdownPresentationStyles::EMPHASIS,
        },
    ];
    // 同一 styled run 在两个相邻授权段上只能分别返回各自交集。
    assert_eq!(
        query_runs_in_range(&runs, &(10..12), 11).unwrap(),
        MarkdownPresentationQuery {
            utf16_range: 10..12,
            styles: MarkdownPresentationStyles::EMPHASIS,
        },
    );
    assert_eq!(
        query_runs_in_range(&runs, &(6..11), 7).unwrap(),
        MarkdownPresentationQuery {
            utf16_range: 6..9,
            styles: MarkdownPresentationStyles::default(),
        },
    );
    assert_eq!(
        query_runs_in_range(&runs, &(20..24), 22).unwrap(),
        MarkdownPresentationQuery {
            utf16_range: 20..24,
            styles: MarkdownPresentationStyles::default(),
        },
    );
    assert!(query_runs_in_range(&runs, &(6..11), 5).is_none());
    assert!(query_runs_in_range(&runs, &(6..11), 11).is_none());
    assert!(query_runs_in_range(&runs, &(11..11), 11).is_none());
}

#[test]
fn delimiter_proof_rejects_visible_or_missing_middle_despite_hidden_endpoints() {
    // 组合样式来自真实 Ready，不依赖 Core 私有 bit 合成接口或伪造原始位值。
    let source = "| h |\n| - |\n";
    let (_directory, document) = document(source);
    let window = ready(&document, 0, 0..source.len());
    let view = NativeMarkdownTableSemanticView::new(&window, &document).unwrap();
    let hidden_styles = view
        .query_published(view.presentation().delimiter_utf16_range.start)
        .unwrap()
        .styles;
    let mut runs = [
        MarkdownPresentationRun {
            utf16_range: 2..4,
            styles: hidden_styles,
        },
        MarkdownPresentationRun {
            utf16_range: 4..8,
            styles: MarkdownPresentationStyles::TABLE_DELIMITER,
        },
        MarkdownPresentationRun {
            utf16_range: 8..10,
            styles: hidden_styles,
        },
    ];
    // 两端都隐藏仍不能遮住中间显露语法；修复后需同一有限范围完整覆盖才成功。
    assert!(!delimiter_runs_are_hidden(&runs, &(2..10)));
    runs[1].styles = hidden_styles;
    assert!(delimiter_runs_are_hidden(&runs, &(2..10)));
    runs[1].utf16_range.end = 7;
    assert!(!delimiter_runs_are_hidden(&runs, &(2..10)));
    assert!(!delimiter_runs_are_hidden(&runs, &(2..2)));
}
