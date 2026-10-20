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
