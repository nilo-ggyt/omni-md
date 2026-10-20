//! 公开来源与组合器签发真实请求；不构造私有票、改内部状态或用相同坐标代替nonce。

use std::{io::Write, ops::Range, sync::Arc};

use super::*;
use crate::markdown_document_buffer::MarkdownViewportSourceRequest;
use crate::{
    MarkdownLayoutFont, MarkdownLineComposeAnswer, MarkdownLineComposeBudget,
    MarkdownLineComposeCancellation, MarkdownLineComposeError, MarkdownLineComposeStyle,
    MarkdownLineComposer, MarkdownLineFontCatalog, MarkdownLineRegisteredFont,
    MarkdownPresentationStyles, MarkdownTextSelection,
};

const ROBOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
));

fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
    MarkdownViewportSourceAuthority {
        context_revision: 57,
        selection: document.selection(),
        cancelled: false,
    }
}

/// 来源只通过公开Leaf工厂推进；publication单独保活的也是原metadata，不是第二份正文。
fn publication(
    document: &MarkdownDocumentBuffer,
    range: Range<usize>,
) -> (
    MarkdownViewportSourceRequest,
    Rc<VerifiedMarkdownViewportSources>,
) {
    let mut request = document
        .viewport_leaf_source_request(range, 57, document.selection())
        .unwrap();
    for _ in 0..10_000 {
        request
            .advance(document, authority(document), 4096)
            .unwrap();
        if let Some(ready) = request
            .take_sources_for(document, authority(document))
            .unwrap()
        {
            return (request, Rc::new(ready));
        }
    }
    panic!("固定短文的真实Leaf来源没有完成");
}

/// 使用未修改的真实SFNT；只提供公开Font角色，不篡改覆盖、查表或制造缺字。
fn font() -> MarkdownLayoutFont {
    let name = read_fonts::FontRef::from_index(ROBOTO, 0)
        .unwrap()
        .table_data(read_fonts::types::Tag::new(b"name"))
        .unwrap()
        .as_bytes()
        .to_vec();
    MarkdownLayoutFont::resolve_name_table_at_size(Arc::new(ROBOTO.to_vec()), &name, &[], 16.0)
        .unwrap()
}

/// 原完整组合入口只排固定短前缀，直到真实内容请求；此处不授予候选的闭项能力。
fn content_request(
    worker: &mut MarkdownLineComposer,
    document: &MarkdownDocumentBuffer,
    ready: &VerifiedMarkdownViewportSources,
    font: &MarkdownLineRegisteredFont,
    target: usize,
) -> MarkdownLineStyleRequest {
    for _ in 0..10_000 {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let range = request.source_range();
                if range.start == target {
                    return request;
                }
                assert!(range.start < target, "不能越过尚未处理的真实内容");
                let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = ready
                    .ordinary_semantics_for(document, authority(document))
                    .unwrap()
                else {
                    panic!("固定粗体/代码段必须有真实RootLeaf覆盖");
                };
                let query = leaf
                    .query_for(document, authority(document), range.start)
                    .unwrap()
                    .unwrap();
                worker
                    .provide_style(
                        request
                            .respond_registered(
                                range.start..range.end.min(query.utf16_range.end),
                                font,
                                MarkdownLineComposeStyle {
                                    presentation: query.styles,
                                    points: 16.0,
                                    language: None,
                                    features: Vec::new(),
                                },
                            )
                            .unwrap(),
                    )
                    .unwrap();
            }
            _ => panic!("固定首段未处理完不能先发布行、控制或EOF"),
        }
    }
    panic!("固定短前缀没有交付真实内容NeedStyle");
}

#[test]
/// 粗体/代码的真实段末可绑定原响应；相同正文位置的另一请求仍不能借此取得封口权。
fn genuine_content_seam_binds_its_response_and_rejects_another_nonce() {
    for (text, content, styles) in [
        ("**AA** tail", 2..4, MarkdownPresentationStyles::STRONG),
        ("`AA` tail", 1..3, MarkdownPresentationStyles::INLINE_CODE),
    ] {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(text.as_bytes()).unwrap();
        let mut document = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
        // 实际选择放在语法之外，原Parser自行收起marker；测试不手工添加Hidden样式。
        document
            .set_selection(MarkdownTextSelection::caret(document.utf16_len()))
            .unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let released = source.resource_observer();
        let mapping = source.original_backing_observer().unwrap();
        let (origin, ready) = publication(&document, 0..document.utf16_len());
        let font = font();
        let catalog = MarkdownLineFontCatalog::new();
        let registered = catalog.register(&font).unwrap();
        // 两个真实Composer共享同一冻结Source，只为取得不同nonce；不是复制或修改私有请求。
        let mut first = MarkdownLineComposer::new(
            source.clone(),
            &source.authority(),
            catalog.clone(),
            200 * 64,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let mut second = MarkdownLineComposer::new(
            source.clone(),
            &source.authority(),
            catalog.clone(),
            200 * 64,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let first_resources = first.worker_resource_observer();
        let second_resources = second.worker_resource_observer();
        // 两方都实际处理完opening marker后才领内容请求，不能靠克隆request伪造跨owner情形。
        let request = content_request(&mut first, &document, &ready, &registered, content.start);
        let foreign = content_request(&mut second, &document, &ready, &registered, content.start);
        let ticket = source
            .verified_root_leaf_style_boundary(
                &document,
                ready.clone(),
                authority(&document),
                &request,
            )
            .unwrap()
            .expect("真实内容末端与closing marker必须有相邻语义");
        let style = MarkdownLineComposeStyle {
            presentation: styles,
            points: 16.0,
            language: None,
            features: Vec::new(),
        };
        assert!(matches!(
            foreign
                .respond_registered(content.clone(), &registered, style.clone())
                .unwrap()
                .with_root_leaf_style_boundary(ticket),
            Err(MarkdownLineComposeError::SourceMismatch)
        ));
        // 失败只归还外来响应；原nonce的真实边界仍能重新从同一出版物领取并被原响应接纳。
        let ticket = source
            .verified_root_leaf_style_boundary(
                &document,
                ready.clone(),
                authority(&document),
                &request,
            )
            .unwrap()
            .unwrap();
        let response = request
            .respond_registered(content.clone(), &registered, style)
            .unwrap()
            .with_root_leaf_style_boundary(ticket)
            .unwrap();
        first.provide_style(response).unwrap();
        // 原完整入口继续处理同一段，接纳边界票不会偷偷消费closing marker或改写源坐标。
        // 此处只验响应来源，不把非Frontier入口当作新候选字体封口已通过。
        let following = content_request(&mut first, &document, &ready, &registered, content.end);
        assert_eq!(following.source_range().start, content.end);
        assert!(!document.can_undo() && !document.can_redo());
        assert_eq!(document.generation(), 0);
        assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
        // 所有真实owner先Drop，才按原收据核FD、映射及Source载荷；弱nonce失效本身不算释放。
        drop((
            following, first, second, registered, catalog, font, ready, origin, source, document,
        ));
        assert!(first_resources.snapshot().unwrap().is_released());
        assert!(second_resources.snapshot().unwrap().is_released());
        assert!(released.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}

#[test]
/// 相同字节的异Doc不能借RootLeaf权限；真实coverage末端和未覆盖的内容也不是语义边界。
fn foreign_document_and_uncovered_edges_never_issue_a_boundary() {
    let text = "**AA** tail";
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(text.as_bytes()).unwrap();
    let mut document = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
    document
        .set_selection(MarkdownTextSelection::caret(document.utf16_len()))
        .unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let mapping = source.original_backing_observer().unwrap();
    let released = source.resource_observer();
    let (origin, ready) = publication(&document, 0..document.utf16_len());
    let font = font();
    let catalog = MarkdownLineFontCatalog::new();
    let registered = catalog.register(&font).unwrap();
    let mut worker = MarkdownLineComposer::new(
        source.clone(),
        &source.authority(),
        catalog.clone(),
        200 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let resources = worker.worker_resource_observer();
    let request = content_request(&mut worker, &document, &ready, &registered, 2);
    // 第一窗口恰在内容末端截断，第二窗口根本不含内容起点；二者都由原SourceRequest实际发布。
    // 即使完整Parser知道closing marker存在，当前coverage没有右query也不能签语义交界。
    for range in [0..4, 6..document.utf16_len()] {
        let (clipped_origin, clipped) = publication(&document, range);
        assert!(
            source
                .verified_root_leaf_style_boundary(
                    &document,
                    clipped,
                    authority(&document),
                    &request
                )
                .unwrap()
                .is_none()
        );
        drop(clipped_origin);
    }
    // 从同一路径重新打开确保字节相同而Doc根身份不同；不能仅比较generation或总长度。
    let mut other = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
    other.set_selection(document.selection()).unwrap();
    assert!(matches!(
        source.verified_root_leaf_style_boundary(
            &other,
            ready.clone(),
            authority(&other),
            &request
        ),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
    assert!(!document.can_undo() && !document.can_redo());
    // 失败不应产生额外正文根或后台工作，当前公开资源owner归还后必须通过原完整释放核验。
    drop((
        request, worker, registered, catalog, font, ready, origin, source, document, other,
    ));
    assert!(resources.snapshot().unwrap().is_released());
    assert!(released.payload_released());
    assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
}

#[test]
/// 选择方向或正文世代变化后须重新取同源语义，不能让旧Ready替新输入签边界票。
fn selection_and_generation_changes_revoke_the_original_semantic_boundary() {
    for edit in [false, true] {
        let text = "**AA** tail";
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(text.as_bytes()).unwrap();
        let mut document = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
        document
            .set_selection(MarkdownTextSelection::caret(document.utf16_len()))
            .unwrap();
        let source = MarkdownLayoutTextSource::capture(&document);
        let mapping = source.original_backing_observer().unwrap();
        let released = source.resource_observer();
        let (origin, ready) = publication(&document, 0..document.utf16_len());
        let font = font();
        let catalog = MarkdownLineFontCatalog::new();
        let registered = catalog.register(&font).unwrap();
        let mut worker = MarkdownLineComposer::new(
            source.clone(),
            &source.authority(),
            catalog.clone(),
            200 * 64,
            MarkdownLineComposeCancellation::new(),
        )
        .unwrap();
        let resources = worker.worker_resource_observer();
        let request = content_request(&mut worker, &document, &ready, &registered, 2);
        // 旧冻结Source故意保持可读；拒绝必须来自当前Doc/选择权限，不能靠提前Drop源掩盖失效。
        if edit {
            document
                .replace_utf16_range(0..0, "X", MarkdownTextSelection::caret(1))
                .unwrap();
        } else {
            // 有向选择本身也是publication权限；正文generation未变化不能替新输入补签旧票。
            document
                .set_selection(MarkdownTextSelection {
                    anchor_utf16: 1,
                    head_utf16: 0,
                })
                .unwrap();
        }
        let expected = if edit {
            MarkdownTableRequestError::SourceSuperseded
        } else {
            MarkdownTableRequestError::PublicationSuperseded
        };
        assert!(
            matches!(source.verified_root_leaf_style_boundary(&document, ready.clone(), authority(&document), &request), Err(error) if error == expected)
        );
        assert_eq!(std::fs::read(file.path()).unwrap(), text.as_bytes());
        // 仅真实编辑产生Undo；这次测试没有保存操作，临时文件仍必须逐字节保持原输入。
        assert_eq!(document.can_undo(), edit);
        drop((
            request, worker, registered, catalog, font, ready, origin, source, document,
        ));
        assert!(resources.snapshot().unwrap().is_released());
        assert!(released.payload_released());
        assert!(mapping.unmap_succeeded() && mapping.mapped_bytes() == 0 && !mapping.failed());
    }
}
