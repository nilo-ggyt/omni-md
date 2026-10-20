//! 原 publication、实际匿名目录及同一显示 Snapshot 的端到端边界；夹具不写入用户文档。

use super::*;
use crate::markdown_document_buffer::MarkdownViewportSourceStep;
use crate::{MarkdownPresentationStyles, MarkdownTextSelection};

/// 真实临时文件由夹具拥有；活动 Doc 与冻结 Source 的生命周期独立核对。
fn document(text: &str) -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), text).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    (file, document)
}

/// 请求必须真正发布原 table window 和 inline runs，不能由测试手工给 raw 格补 Ready。
fn publication(
    document: &MarkdownDocumentBuffer,
) -> (
    Rc<VerifiedMarkdownViewportSources>,
    MarkdownViewportSourceAuthority,
) {
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 91,
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
        match request.advance(document, authority, 4096).unwrap() {
            MarkdownViewportSourceStep::Progress { .. } => {}
            MarkdownViewportSourceStep::SourcesAvailable { .. } => {
                if let Some(sources) = request.take_sources_for(document, authority).unwrap() {
                    return (Rc::new(sources), authority);
                }
            }
        }
    }
}

/// 生产入口按真实量子推进，转换输入与输出合计各收费一次，不隐藏整格扫描。
fn finish(
    preparation: &mut MarkdownTableCellDisplayPreparation,
    document: &MarkdownDocumentBuffer,
) -> MarkdownTableCellDisplaySource {
    loop {
        let step = preparation.advance(document, false).unwrap();
        let (ready, work) = match step {
            MarkdownTableCellDisplayPreparationStep::Progress { work } => (false, work),
            MarkdownTableCellDisplayPreparationStep::Ready { work } => (true, work),
        };
        assert!(work.source_bytes <= MARKDOWN_LAYOUT_SOURCE_BYTES);
        assert!(work.copied_source_bytes <= MARKDOWN_LAYOUT_SOURCE_BYTES);
        assert!(work.structural_transitions <= 512);
        if ready {
            return preparation.take_ready_for(document).unwrap().unwrap();
        }
    }
}

/// 短格的原 Parser 样式、转义来源和真实显示字节保持一致，半代理不能成为编辑端点。
#[test]
fn real_publication_converts_inline_roles_and_maps_display_back_to_original_cell() {
    let text = "| H | C |\n| --- | --- |\n| **粗体** 与 *斜体* [链接](https://example.com) `a\\|b` 中😀e\u{301} שלום | other |\n";
    let (_file, document) = document(text);
    let (published, authority) = publication(&document);
    let mut preparation =
        MarkdownTableCellDisplayPreparation::new(&document, published, authority, 0, 1, 0).unwrap();
    let display = finish(&mut preparation, &document);
    let expected = "粗体 与 斜体 链接 a|b 中😀e\u{301} שלום";
    let mut bytes = Vec::new(); // 有限小夹具作为独立文本 oracle，不是产品的整格读取方式。
    display
        .source()
        .write_utf16_range_to(0..display.source().utf16_len(), &mut bytes)
        .unwrap();
    assert_eq!(bytes, expected.as_bytes());
    assert_eq!(
        display.source().domain(),
        MarkdownLayoutTextSourceDomain::ReadOnlyCell
    );
    assert!(display.source().cell_draft_authority().is_none());
    assert!(
        display
            .styles_at(0)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::STRONG)
    );
    let emphasis = expected[..expected.find("斜体").unwrap()]
        .encode_utf16()
        .count();
    assert!(
        display
            .styles_at(emphasis)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::EMPHASIS)
    );
    let link = expected[..expected.find("链接").unwrap()]
        .encode_utf16()
        .count();
    assert!(
        display
            .styles_at(link)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::LINK)
    );
    let code = expected[..expected.find("a|b").unwrap()]
        .encode_utf16()
        .count();
    assert!(
        display
            .styles_at(code)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::INLINE_CODE)
    );
    let pipe = text[..text.find("|b`").unwrap()].encode_utf16().count();
    assert_eq!(
        display
            .display_to_canonical(code + 1, MarkdownComposedLineAffinity::Downstream)
            .unwrap(),
        pipe
    );
    assert_eq!(
        display
            .canonical_to_display(pipe - 1, MarkdownComposedLineAffinity::Downstream)
            .unwrap(),
        Some(code + 1)
    );
    // 标签末端的两侧原坐标确实跨隐藏 URL；同一显示 caret 不授予链接范围吞掉它的权限。
    let link_end = link + "链接".encode_utf16().count();
    let canonical_end = text[..text.find("](https").unwrap()].encode_utf16().count();
    assert_eq!(
        display
            .display_to_canonical(link_end, MarkdownComposedLineAffinity::Upstream)
            .unwrap(),
        canonical_end
    );
    assert!(
        display
            .display_to_canonical(link_end, MarkdownComposedLineAffinity::Downstream)
            .unwrap()
            > canonical_end
    );
    let emoji = expected[..expected.find('😀').unwrap()]
        .encode_utf16()
        .count();
    assert_eq!(
        display.source().byte_offset_for_utf16(emoji + 1),
        Err(MarkdownDocumentBufferError::NotUnicodeBoundary)
    );
    assert!(
        display
            .styles_at(display.source().utf16_len())
            .unwrap()
            .is_none()
    );
    assert!(preparation.take_ready_for(&document).unwrap().is_none());
    display.require_for(&document).unwrap();
}

/// 大量真实删除断点只放进原分页目录；最后一个显示来源释放后 FD、页和原映射全归还。
#[test]
fn real_projection_origin_directory_stays_paged_and_source_owns_only_shared_bytes() {
    let text = format!(
        "| H | C |\n| --- | --- |\n| {} | other |\n",
        "\\*".repeat(2200)
    );
    let (_file, document) = document(&text);
    let before = document.storage_profile();
    let (published, authority) = publication(&document);
    let mut preparation =
        MarkdownTableCellDisplayPreparation::new(&document, published, authority, 0, 1, 0).unwrap();
    let display = finish(&mut preparation, &document);
    let observer = display.resource_observer();
    let backing = display.source().original_backing_observer().unwrap();
    assert_eq!(display.source().byte_len(), 2200);
    assert_eq!(document.storage_profile(), before);
    assert!(!document.has_cell_draft());
    assert!(observer.snapshot().origin_file_open);
    assert_eq!(observer.snapshot().origin_buffer_bytes, 0);
    assert!(observer.snapshot().origin_peak_buffer_bytes <= 16 * 1024);
    drop(preparation);
    drop(document);
    let mut copied = 0;
    for start in (0..2200).step_by(127) {
        display
            .source()
            .visit_byte_range(start..(start + 127).min(2200), |bytes| {
                assert!(bytes.iter().all(|byte| *byte == b'*'));
                copied += bytes.len();
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(copied, 2200);
    assert!(!observer.snapshot().source_payload_released);
    drop(display);
    let released = observer.snapshot();
    assert!(
        released.source_payload_released
            && !released.origin_file_open
            && released.origin_buffer_bytes == 0
    );
    assert!(!released.origin_cleanup_failed && !released.origin_reader_failed);
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

/// 冻结显示字节不恢复旧选择/代次的活动权限；取消和实际源错误都释放尚未发布的 writer。
#[test]
fn display_preparation_rejects_cancelled_old_publications_and_preserves_real_source_failure() {
    let text = "| H | C |\n| --- | --- |\n| **中文** | other |\n";
    let (_file, mut document) = document(text);
    let (published, authority) = publication(&document);
    let mut preparation =
        MarkdownTableCellDisplayPreparation::new(&document, published.clone(), authority, 0, 1, 0)
            .unwrap();
    assert!(matches!(
        preparation.advance(&document, true),
        Err(MarkdownTableRequestError::Cancelled)
    ));
    assert!(matches!(
        preparation.take_ready_for(&document),
        Err(MarkdownTableRequestError::Cancelled)
    ));
    let mut preparation =
        MarkdownTableCellDisplayPreparation::new(&document, published, authority, 0, 1, 0).unwrap();
    let display = finish(&mut preparation, &document);
    document
        .set_selection(MarkdownTextSelection::caret(1))
        .unwrap();
    assert!(matches!(
        display.require_for(&document),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    document.set_selection(authority.selection).unwrap();
    document.fail_next_original_reclaim_for_test();
    assert_eq!(
        document.reclaim_original_read_windows_for_test(),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert_eq!(
        display.source().byte_offset_for_utf16(0),
        Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
            libc::EIO
        ))
    );
    assert!(matches!(
        display.require_for(&document),
        Err(MarkdownTableRequestError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    ));
}

/// 正式 RootLeaf 首份来源先交结构行，Cell 后续必须串行借同一个原请求完成 inline 与匿名显示目录。
fn leaf_publication(
    document: &MarkdownDocumentBuffer,
) -> (
    MarkdownViewportSourceRequest,
    Rc<VerifiedMarkdownViewportSources>,
    MarkdownViewportSourceAuthority,
) {
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 92,
        selection: document.selection(),
        cancelled: false,
    };
    let mut request = document
        .viewport_leaf_source_request(
            0..document.utf16_len().min(128),
            authority.context_revision,
            authority.selection,
        )
        .unwrap();
    loop {
        match request.advance(document, authority, 4096).unwrap() {
            MarkdownViewportSourceStep::Progress { .. } => (),
            MarkdownViewportSourceStep::SourcesAvailable { .. } => {
                if let Some(publication) = request.take_sources_for(document, authority).unwrap() {
                    return (request, Rc::new(publication), authority);
                }
            }
        }
    }
}

/// 真正 CellInline 完成之前无显示 Ready；每个阶段分别服从原 4KiB 源工作与有界控制量子。
fn finish_leaf(
    preparation: &mut MarkdownTableCellDisplayPreparation,
    document: &MarkdownDocumentBuffer,
    request: &mut MarkdownViewportSourceRequest,
) -> MarkdownTableCellDisplaySource {
    assert!(preparation.take_ready_for(document).unwrap().is_none());
    loop {
        let step = preparation
            .advance_with_source_request(document, request, false)
            .unwrap();
        let (ready, work) = match step {
            MarkdownTableCellDisplayPreparationStep::Progress { work } => (false, work),
            MarkdownTableCellDisplayPreparationStep::Ready { work } => (true, work),
        };
        assert!(
            work.source_bytes <= MARKDOWN_LAYOUT_SOURCE_BYTES,
            "{work:?}"
        );
        assert!(
            work.copied_source_bytes <= MARKDOWN_LAYOUT_SOURCE_BYTES,
            "{work:?}"
        );
        assert!(
            work.structural_transitions <= MARKDOWN_LAYOUT_SOURCE_BYTES,
            "{work:?}"
        );
        if ready {
            return preparation.take_ready_for(document).unwrap().unwrap();
        }
    }
}

#[test]
fn structural_row_uses_same_request_real_cell_parser_reference_links_and_display_converter() {
    let text = "| H | C |\r\n| --- | --- |\r\n| **粗体** *斜体* [真实链接][地址] `a\\|b` | tail |\r\n\r\n[地址]: https://example.com/actual \"实际标题\"\r\n";
    let (_file, document) = document(text);
    let (mut request, publication, authority) = leaf_publication(&document);
    assert!(
        publication
            .table_sources_for(&document, authority)
            .unwrap()
            .windows_for(&document)
            .unwrap()
            .len()
            == 0
    );
    let row_start = text.find("| **").unwrap();
    let row = publication
        .table_row_at_for(&document, authority, row_start)
        .unwrap()
        .unwrap();
    let mut preparation =
        MarkdownTableCellDisplayPreparation::from_row_source(&document, row, authority, 0).unwrap();
    let display = finish_leaf(&mut preparation, &document, &mut request);
    let expected = "粗体 斜体 真实链接 a|b";
    let mut bytes = Vec::new(); // 只在这个独立短夹具保有限 oracle 文本，产品不读整格。
    display
        .source()
        .write_utf16_range_to(0..display.source().utf16_len(), &mut bytes)
        .unwrap();
    assert_eq!(bytes, expected.as_bytes());
    let index = text[..text.find("真实链接").unwrap()]
        .encode_utf16()
        .count();
    let link = display.link_at_for(&document, index).unwrap().unwrap();
    assert_eq!(
        link.destination.clone().into_string(),
        "https://example.com/actual"
    );
    assert!(
        display
            .styles_at(0)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::STRONG)
    );
    assert!(request.total_work().parser_input_bytes > 0);
    request.cancel();
    display.require_for(&document).unwrap(); // 取消只释放作业，外带同源不可变发布仍可读。
}

#[test]
fn giant_first_row_plain_and_code_use_bounded_same_owner_semantics_and_disk_display() {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let header = "| H | C |\r\n| --- | --- |\r\n";
    file.write_all(header.as_bytes()).unwrap();
    file.write_all(b"|").unwrap();
    let page = [b'a'; 4096];
    for _ in 0..80 {
        file.write_all(&page).unwrap();
    }
    file.write_all(b"|`").unwrap();
    for _ in 0..80 {
        file.write_all(&page).unwrap();
    }
    file.write_all(b"`|\r\n").unwrap();
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    let (mut request, publication, authority) = leaf_publication(&document);
    let row = publication
        .table_row_at_for(&document, authority, header.len())
        .unwrap()
        .unwrap();
    for column in 0..2 {
        let mut preparation = MarkdownTableCellDisplayPreparation::from_row_source(
            &document,
            row.clone(),
            authority,
            column,
        )
        .unwrap();
        let display = finish_leaf(&mut preparation, &document, &mut request);
        assert_eq!(display.source().utf16_len(), 80 * page.len());
        let mut prefix = Vec::new(); // 只有固定 32B 真实读取；不创建超长格的测试副本。
        display
            .source()
            .write_utf16_range_to(0..32, &mut prefix)
            .unwrap();
        assert_eq!(prefix, &page[..32]);
        let styles = display.styles_at(0).unwrap().unwrap().styles;
        assert!(styles.contains(MarkdownPresentationStyles::TABLE));
        assert_eq!(
            styles.contains(MarkdownPresentationStyles::INLINE_CODE),
            column == 1
        );
        let observer = display.resource_observer();
        drop(display);
        drop(preparation);
        let resources = observer.snapshot();
        assert!(resources.source_payload_released);
        assert!(!resources.origin_file_open);
        assert_eq!(resources.origin_buffer_bytes, 0);
        assert!(!resources.origin_cleanup_failed);
    }
    request.cancel();
}

/// 未借来源和真正已开始的Inline都可取消；后者必须交回原槽，后续真实列仍由同请求完成。
#[test]
fn cancelling_unstarted_or_started_cell_inline_keeps_original_request_usable() {
    let text = "| H | C |\r\n| --- | --- |\r\n| **中文** 与 `a\\|b` | tail |\r\n";
    for started in [false, true] {
        let (file, document) = document(text);
        let backing = document.original_backing_observer().unwrap();
        let (mut request, publication, authority) = leaf_publication(&document);
        let row = publication
            .table_row_at_for(&document, authority, text.find("| **").unwrap())
            .unwrap()
            .unwrap();
        let mut preparation = MarkdownTableCellDisplayPreparation::from_row_source(
            &document,
            row.clone(),
            authority,
            0,
        )
        .unwrap();
        if started {
            assert!(matches!(
                preparation
                    .advance_with_source_request(&document, &mut request, false)
                    .unwrap(),
                MarkdownTableCellDisplayPreparationStep::Progress { .. }
            )); // 这是原生产入口真正开始Inline，测试不改阶段或制造Ready。
        }
        loop {
            let step = preparation
                .cancel_with_source_request(&document, &mut request)
                .unwrap();
            let (ready, work) = match step {
                MarkdownTableCellDisplayPreparationStep::Progress { work } => (false, work),
                MarkdownTableCellDisplayPreparationStep::Ready { work } => (true, work),
            };
            assert!(work.source_bytes <= MARKDOWN_LAYOUT_SOURCE_BYTES);
            if !started {
                assert_eq!(work, SemanticWork::default()); // 未借Request的准备取消不应启动任何来源读取。
            } else {
                // 已借出的原语义必须完成并归槽；短格的真实BoundedInline输入复制也属此计量，
                // 不能把它误当Display转换或隐藏费用。两类真实读量仍保原4KiB量子。
                assert!(work.copied_source_bytes <= work.source_bytes);
                assert!(work.copied_source_bytes <= MARKDOWN_LAYOUT_SOURCE_BYTES);
            }
            assert!(preparation.resource_observer().is_none());
            if ready {
                break;
            }
        }
        assert!(matches!(
            preparation.take_ready_for(&document),
            Err(MarkdownTableRequestError::Cancelled)
        ));
        let mut next =
            MarkdownTableCellDisplayPreparation::from_row_source(&document, row, authority, 1)
                .unwrap();
        let display = finish_leaf(&mut next, &document, &mut request);
        let mut bytes = Vec::new(); // 后继短格的独立可见结果证明原请求仍能实际准备，而非只检查内部槽位。
        display
            .source()
            .write_utf16_range_to(0..display.source().utf16_len(), &mut bytes)
            .unwrap();
        assert_eq!(bytes, b"tail");
        assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
        let observer = display.resource_observer();
        drop(display);
        drop(next);
        drop(preparation);
        drop(publication);
        drop(request);
        drop(document);
        let released = observer.snapshot();
        assert!(released.source_payload_released && !released.origin_file_open);
        assert_eq!(released.origin_buffer_bytes, 0);
        assert!(!released.origin_cleanup_failed && !released.origin_reader_failed);
        assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
    }
}

/// 已存在但尚未封存的目录必须立即关闭；取消不能为“完成”再转换剩余转义内容。
#[test]
fn cancelling_unpublished_display_closes_real_writer_without_converting_the_tail() {
    let text = format!(
        "| H | C |\n| --- | --- |\n| {} | tail |\n",
        "\\*".repeat(2200)
    );
    let (file, document) = document(&text);
    let (mut request, publication, authority) = leaf_publication(&document);
    let row = publication
        .table_row_at_for(&document, authority, text.find("| \\").unwrap())
        .unwrap()
        .unwrap();
    let mut preparation =
        MarkdownTableCellDisplayPreparation::from_row_source(&document, row.clone(), authority, 0)
            .unwrap();
    let observer = loop {
        let step = preparation
            .advance_with_source_request(&document, &mut request, false)
            .unwrap();
        assert!(matches!(
            step,
            MarkdownTableCellDisplayPreparationStep::Progress { .. }
        ));
        if let Some(observer) = preparation.resource_observer()
            && observer.snapshot().origin_file_open
        {
            break observer; // 只等真实FD打开，不读取阶段、私有builder或虚构资源句柄。
        }
    };
    assert!(!observer.snapshot().source_payload_released);
    let before = request.total_work();
    assert!(matches!(
        preparation.cancel_with_source_request(&document, &mut request).unwrap(),
        MarkdownTableCellDisplayPreparationStep::Ready { work } if work == SemanticWork::default()
    ));
    assert_eq!(request.total_work(), before); // 已归槽的Display取消没有再扫源、解析或转换尾部。
    let released = observer.snapshot();
    assert!(released.source_payload_released && !released.origin_file_open);
    assert_eq!(released.origin_buffer_bytes, 0);
    assert!(!released.origin_cleanup_failed && !released.origin_reader_failed);
    let mut next =
        MarkdownTableCellDisplayPreparation::from_row_source(&document, row, authority, 1).unwrap();
    let display = finish_leaf(&mut next, &document, &mut request);
    let mut bytes = Vec::new();
    display
        .source()
        .write_utf16_range_to(0..display.source().utf16_len(), &mut bytes)
        .unwrap();
    assert_eq!(bytes, b"tail");
    assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
}
