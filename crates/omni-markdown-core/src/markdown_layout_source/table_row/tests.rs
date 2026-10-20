//! 原 physical 行与唯一 Source 的实际关联；本批不把端点结构票当成完整 Grid 或 Unicode 结果。

use super::*;
use crate::MarkdownTextSelection;
use crate::markdown_document_buffer::MarkdownViewportSourceStep;

/// 合成文件不涉及现有笔记；NamedTempFile 和映射均随测试作用域归还。
fn open_document(text: &str) -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), text).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    (file, document)
}

/// 只消费原 SourcesAvailable 的真实有限 table publication，不手工拼 row/cell 范围。
fn publication(
    document: &MarkdownDocumentBuffer,
) -> (
    Rc<VerifiedMarkdownViewportSources>,
    MarkdownViewportSourceAuthority,
) {
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 92,
        selection: document.selection(),
        cancelled: false,
    };
    let mut request = document
        .viewport_source_request(
            0..document.utf16_len(),
            authority.context_revision,
            authority.selection,
        )
        .unwrap();
    loop {
        if matches!(
            request.advance(document, authority, 4096).unwrap(),
            MarkdownViewportSourceStep::SourcesAvailable { .. }
        ) && let Some(ready) = request.take_sources_for(document, authority).unwrap()
        {
            return (Rc::new(ready), authority);
        }
    }
}

/// 表头和 delimiter 是一个真实 grid 首行，CRLF 和最终未终止正文的 EOF 都保准确范围。
#[test]
fn verified_flow_rows_keep_complete_physical_crlf_and_header_delimiter() {
    let text = "| H | C |\r\n| --- | --- |\r\n| 中文😀 | `a\\|b` |\r\n| last | final |";
    let (_file, document) = open_document(text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let (published, authority) = publication(&document);
    let header = source
        .verified_table_layout_row(&document, published.clone(), authority, 0, 0)
        .unwrap();
    let first = source
        .verified_table_layout_row(&document, published.clone(), authority, 0, 1)
        .unwrap();
    let last = source
        .verified_table_layout_row(&document, published, authority, 0, 2)
        .unwrap();
    let body_start = text[..text.find("| 中文").unwrap()].encode_utf16().count();
    let last_start = text[..text.find("| last").unwrap()].encode_utf16().count();
    assert_eq!(header.source_range(), 0..body_start);
    assert_eq!(first.source_range(), body_start..last_start);
    assert_eq!(last.source_range(), last_start..document.utf16_len());
    assert_eq!(first.coordinate(), (0, 1));
    assert_eq!(first.columns(), 2);
    let mut preparation = first.prepare_cell(&document, &source, 1).unwrap();
    loop {
        if matches!(
            preparation.advance(&document, false).unwrap(),
            MarkdownTableCellDisplayPreparationStep::Ready { .. }
        ) {
            break;
        }
    }
    let display = preparation.take_ready_for(&document).unwrap().unwrap();
    let mut bytes = Vec::new(); // 仅三字符小格 oracle，实际来源仍使用原 4 KiB cursor。
    display
        .source()
        .write_utf16_range_to(0..display.source().utf16_len(), &mut bytes)
        .unwrap();
    assert_eq!(bytes, b"a|b");
}

#[test]
/// Unicode正文、两张表和真实表间隙共用一个动态目录。每次追加只授权已出版行，
/// 首尾包络不能吞掉间隙、提前授权后续行，或将字节坐标误作UTF16坐标。
fn published_row_prefix_keeps_unicode_gaps_and_releases_its_actual_file() {
    let text = concat!(
        "前言😀\n\n| 一 | 二 |\r\n| --- | --- |\r\n| 中文 | 😀 |\r\n",
        "\n中间😀普通正文\n\n| 三 | 四 |\n| --- | --- |\n| 尾行 | 完 |\n\n后记😀",
    );
    let (file, document) = open_document(text);
    let backing = document.original_backing_observer().unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let (published, authority) = publication(&document);
    let rows = [
        source
            .verified_table_layout_row(&document, published.clone(), authority, 0, 0)
            .unwrap(),
        source
            .verified_table_layout_row(&document, published.clone(), authority, 0, 1)
            .unwrap(),
        source
            .verified_table_layout_row(&document, published.clone(), authority, 1, 0)
            .unwrap(),
        source
            .verified_table_layout_row(&document, published.clone(), authority, 1, 1)
            .unwrap(),
    ];
    // 期望边界来自固定原文的独立字面与真实EOL，不从目录、包络或查询结果反造。
    let at = |literal: &str| text.find(literal).unwrap();
    let expected_bytes = [
        at("| 一")..at("| 中文"),
        at("| 中文")..at("\n中间"),
        at("| 三")..at("| 尾行"),
        at("| 尾行")..at("\n后记"),
    ];
    let utf16 = |byte| text[..byte].encode_utf16().count();
    let expected_units = expected_bytes
        .each_ref()
        .map(|range| utf16(range.start)..utf16(range.end));
    for (row, expected) in rows.iter().zip(&expected_units) {
        assert_eq!(row.source_range(), *expected);
    }
    let mut writer = super::super::table_rows::TablePlanWriter::new(&source);
    let plan = writer.plan();
    let resources = plan.resource_observer();
    for (index, row) in rows.iter().enumerate() {
        while !writer.advance_row(&source, row).unwrap().ready {}
        // 前缀逐次增长期间，字符与caret边界保留原不同端点规则；表间正文始终可布局。
        // 所有查询走真实原文件；不注入记录、检查读取次数或直接改写私有publication。
        for (is_utf16, ranges, length) in [
            (false, &expected_bytes, text.len()),
            (true, &expected_units, document.utf16_len()),
        ] {
            for position in 0..=length {
                let prefix = &ranges[..=index];
                assert_eq!(
                    plan.contains_character(position, is_utf16).unwrap(),
                    prefix.iter().any(|range| range.contains(&position)),
                    "字符权限必须只属于已出版行：prefix={index} utf16={is_utf16} position={position}",
                );
                assert_eq!(
                    plan.contains(position, is_utf16).unwrap(),
                    prefix
                        .iter()
                        .any(|range| range.start < position && position < range.end),
                    "硬端点仍保留原caret权限：prefix={index} utf16={is_utf16} position={position}",
                );
            }
        }
        let gap = utf16(at("中间"));
        assert!(!plan.intersects(&(gap..gap + 2)).unwrap());
        assert!(!plan.intersects(&(0..expected_units[0].start)).unwrap());
        assert!(
            !plan
                .intersects(&(expected_units[index].end..document.utf16_len()))
                .unwrap()
        );
        assert!(plan.intersects(&expected_units[index]).unwrap());
    }
    assert_eq!(document.generation(), 0);
    assert_eq!(document.selection(), MarkdownTextSelection::caret(0));
    assert!(!document.is_dirty() && !document.can_undo() && !document.can_redo());
    assert!(document.original_file_identity_is_unchanged());
    assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
    drop((writer, plan, rows, published, source, document));
    let closed = resources.snapshot();
    assert!(closed.owner_released);
    let closed = closed.file.unwrap();
    assert!(!closed.file_open && !closed.cleanup_failed && !closed.reader_failed);
    #[cfg(feature = "paged-shaping-frontier")]
    assert_eq!(closed.prefix_readers, 0);
    assert_eq!(closed.buffer_bytes, 0);
    assert_eq!(closed.mapped_bytes, 0);
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

/// 同一 Doc 的新 capture 也有不同的 Source 身份，另一 Doc 和只读格更不能取得原 flow 票。
#[test]
fn flow_row_requires_original_canonical_source_and_current_publication() {
    let text = "| H | C |\n| --- | --- |\n| a | b |\n";
    let (_file, mut document) = open_document(text);
    let (_other_file, other) = open_document(text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let other_source = MarkdownLayoutTextSource::capture(&other);
    let (published, authority) = publication(&document);
    assert!(matches!(
        other_source.verified_table_layout_row(&document, published.clone(), authority, 0, 1),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    let row = source
        .verified_table_layout_row(&document, published, authority, 0, 1)
        .unwrap();
    let fresh = MarkdownLayoutTextSource::capture(&document);
    assert!(matches!(
        row.require_for(&document, &fresh),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert!(matches!(
        row.prepare_cell(&document, &fresh, 0),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    document
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    assert!(matches!(
        row.require_for(&document, &source),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    document
        .insert_utf16(0, "前", MarkdownTextSelection::caret(1))
        .unwrap();
    assert!(matches!(
        row.require_for(&document, &source),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
}

/// 同一源的永久 EIO 先于旧行/Source 身份错误；结构票不能通过相同端点恢复字节访问。
#[test]
fn row_factory_and_borrow_preserve_original_source_failure() {
    let text = "| H | C |\n| --- | --- |\n| a | b |\n";
    let (_file, document) = open_document(text);
    let source = MarkdownLayoutTextSource::capture(&document);
    let (published, authority) = publication(&document);
    let row = source
        .verified_table_layout_row(&document, published.clone(), authority, 0, 1)
        .unwrap();
    document.fail_next_original_reclaim_for_test();
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(matches!(
        row.require_for(&document, &source),
        Err(MarkdownTableRequestError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
    assert!(matches!(
        source.verified_table_layout_row(&document, published, authority, 0, 1),
        Err(MarkdownTableRequestError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
}
