//! 真实来源请求与固定完整Parser作为独立oracle；所有全文样本和oracle只存在于测试。

mod atx;
mod partial;
mod production;

use super::*;
use crate::markdown_viewport_presentation::MarkdownViewportPresentation;
use pulldown_cmark::{Event, Parser, Tag};

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("有界叶块验收.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}
fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
    MarkdownViewportSourceAuthority {
        context_revision: 57,
        selection: document.selection(),
        cancelled: false,
    }
}
fn units(source: &str, range: Range<usize>) -> Range<usize> {
    source[..range.start].encode_utf16().count()..source[..range.end].encode_utf16().count()
}
fn first_leaf_range(source: &str) -> Range<usize> {
    Parser::new_ext(
        source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| {
        matches!(event, Event::Start(Tag::CodeBlock(_)) | Event::Rule).then_some(range)
    })
    .unwrap()
}
fn source_request(
    document: &MarkdownDocumentBuffer,
    requested: Range<usize>,
) -> (
    MarkdownViewportSourceRequest,
    VerifiedMarkdownViewportSources,
) {
    let mut request = document
        .viewport_source_request(requested, 57, document.selection())
        .unwrap();
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        request
            .advance(document, authority(document), 4096)
            .unwrap();
        if let Some(sources) = request
            .take_sources_for(document, authority(document))
            .unwrap()
        {
            return (request, sources);
        }
    }
    panic!("真实Global来源请求未完成");
}
fn prepare_job(
    request: &mut MarkdownViewportSourceRequest,
    sources: &VerifiedMarkdownViewportSources,
    document: &MarkdownDocumentBuffer,
) -> RootLeafSemanticJob {
    let mut job = RootLeafSemanticJob::new(
        sources.publication.source.clone(),
        57,
        document.selection(),
        sources.requested.clone(),
        request.batch.source_semantic_quota().unwrap(),
    )
    .unwrap();
    let construction = RootLeafSemanticJob::construction_work();
    assert_step(construction);
    request.batch.account_source_work(construction).unwrap();
    while job.preparing() {
        let work = job
            .advance(
                document,
                authority(document),
                false,
                4096,
                request.remaining_source_limits().unwrap(),
                &mut request.semantic,
            )
            .unwrap();
        assert_step(work);
        request.batch.account_source_work(work).unwrap();
    }
    for record in &sources.records {
        let work = job.offer_record(record).unwrap();
        assert_step(work);
        request.batch.account_source_work(work).unwrap();
    }
    job
}
fn assert_step(work: SemanticWork) {
    assert!(work.source_bytes <= 4096);
    assert!(work.structural_transitions <= 4096);
    assert!(work.metadata_bytes <= 4096);
    assert_eq!(work.copied_source_bytes, 0);
    assert_eq!(work.parser_input_bytes, 0);
    assert_eq!(work.allocated_source_capacity, 0);
}
fn finish_job(
    request: &mut MarkdownViewportSourceRequest,
    job: &mut RootLeafSemanticJob,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        let work = job.advance(
            document,
            authority(document),
            true,
            4096,
            request.remaining_source_limits().unwrap(),
            &mut request.semantic,
        )?;
        assert_step(work);
        request.batch.account_source_work(work)?;
        if job.done() {
            return Ok(());
        }
    }
    panic!("Root Leaf有限job未完成");
}
fn publish(
    request: &mut MarkdownViewportSourceRequest,
    job: &mut RootLeafSemanticJob,
    sources: &VerifiedMarkdownViewportSources,
    document: &MarkdownDocumentBuffer,
) -> RootLeafSemanticPublication {
    let (result, work) = job
        .publish(
            document,
            authority(document),
            Rc::downgrade(&sources.publication),
            4096,
        )
        .unwrap();
    assert_step(work);
    request.batch.account_source_work(work).unwrap();
    result
}
fn assert_oracle(
    ready: &VerifiedMarkdownRootLeafViewport,
    document: &MarkdownDocumentBuffer,
    source: &str,
    requested: &Range<usize>,
) {
    let mut baseline = MarkdownViewportPresentation::default();
    baseline.set_active_selection(ordered_selection(document.selection()));
    baseline
        .rebuild(document.generation(), 0..document.utf16_len(), source)
        .unwrap();
    assert_eq!(
        ready.coverage_for(document, authority(document)).unwrap(),
        requested
    );
    assert_eq!(
        ready
            .runs_for(document, authority(document))
            .unwrap()
            .count(),
        ready.run_count()
    );
    for index in requested.clone() {
        assert_eq!(
            ready
                .query_for(document, authority(document), index)
                .unwrap()
                .unwrap()
                .styles,
            baseline.query(index).unwrap().styles,
            "原源={source:?}, index={index}, selection={:?}",
            document.selection()
        );
    }
    assert!(
        ready
            .query_for(document, authority(document), requested.end)
            .unwrap()
            .is_none()
    );
    if requested.start != 0 {
        assert!(
            ready
                .query_for(document, authority(document), requested.start - 1)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn root_code_and_rule_match_complete_projection_with_unicode_selection_and_global_underline() {
    for source in [
        "前言\n\n``` rust\n中文😀代码<u>下划线</u>\n```\n\n结尾\n",
        "前言\r\n\r\n  ~~~~ text\r\n中文😀\r\n  ~~~~~   \r\n\r\n结尾\r\n",
        "前言\n\n    中文😀\n    第二行<u>字面标记</u>\n\n结尾\n",
        "前言\n\n  ***  \n\n结尾\n",
        "前言\n\n```\n正文\r仍是同一raw-LF行\n```\n\n结尾\n",
        "<u>前面省略的块\n\n```text\n中文😀位于远处真实配对内部\n```\n\n后面省略的块</u>\n",
    ] {
        let requested = units(source, first_leaf_range(source));
        for selection in [
            MarkdownTextSelection::caret(requested.start),
            MarkdownTextSelection::caret(source.encode_utf16().count()),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (_directory, mut doc) = document(source);
            doc.set_selection(selection).unwrap();
            let (mut request, sources) = source_request(&doc, requested.clone());
            let mut job = prepare_job(&mut request, &sources, &doc);
            finish_job(&mut request, &mut job, &doc).unwrap();
            let RootLeafSemanticPublication::Ready(ready) =
                publish(&mut request, &mut job, &sources, &doc)
            else {
                panic!("真实Root code/rule不应缺少语义");
            };
            assert_oracle(&ready, &doc, source, &requested);
            assert!(ready.run_count() > 0);
            assert!(
                ready.retained_metadata()
                    >= std::mem::size_of::<VerifiedMarkdownRootLeafViewport>()
            );
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);
        }
    }
}

#[test]
fn true_eof_fence_has_no_fabricated_closer_and_legacy_false_markers_stay_pending() {
    for source in [
        "前言\n\n```text\n中文😀",
        "前言\n\n~~~text\n中文😀\n",
        "前言\n\n```",
    ] {
        let requested = units(source, first_leaf_range(source));
        let (_directory, doc) = document(source);
        let (mut request, sources) = source_request(&doc, requested.clone());
        let mut job = prepare_job(&mut request, &sources, &doc);
        finish_job(&mut request, &mut job, &doc).unwrap();
        let RootLeafSemanticPublication::Ready(ready) =
            publish(&mut request, &mut job, &sources, &doc)
        else {
            panic!("无假末行marker的真实EOF应Ready");
        };
        assert_oracle(&ready, &doc, source, &requested);
        assert_eq!(
            ready.block_kind_for(&doc, authority(&doc)).unwrap(),
            LeafBlockKind::FencedCode
        );
    }
    for source in [
        "前言\n\n```\n正文\n```不是closer",
        "前言\n\n````\n正文\n```",
        "前言\n\n```\n正文\n~~~",
    ] {
        let requested = units(source, first_leaf_range(source));
        let (_directory, doc) = document(source);
        let (mut request, sources) = source_request(&doc, requested);
        let mut job = prepare_job(&mut request, &sources, &doc);
        finish_job(&mut request, &mut job, &doc).unwrap();
        let RootLeafSemanticPublication::Pending(reason) =
            publish(&mut request, &mut job, &sources, &doc)
        else {
            panic!("旧首末行helper与真实closer不同时禁止伪兼容Ready");
        };
        assert_eq!(
            reason,
            MarkdownRootLeafPendingReason::LegacyFenceBoundaryMismatch
        );
    }
}

#[test]
fn giant_single_code_line_keeps_finite_coverage_and_zero_source_copy() {
    let source = format!("前言\n\n```text\n{}\n```\n\n末尾\n", "中".repeat(100_000));
    let (_directory, doc) = document(&source);
    let start = 50_000;
    let requested = start..start + 17;
    let (mut request, sources) = source_request(&doc, requested.clone());
    let mut job = prepare_job(&mut request, &sources, &doc);
    finish_job(&mut request, &mut job, &doc).unwrap();
    let RootLeafSemanticPublication::Ready(ready) = publish(&mut request, &mut job, &sources, &doc)
    else {
        panic!("巨型code正文的有限交集应保持Ready");
    };
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)).unwrap(),
        &requested
    );
    for index in requested {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles,
            MarkdownPresentationStyles::FENCED_CODE
        );
    }
    assert_eq!(ready.run_count(), 1);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn paged_runs_consume_original_joint_quota_and_limit_before_page_allocation() {
    let body = "<u>x</u>".repeat(120);
    let source = format!("前言\n\n```\n{body}\n```\n\n尾\n");
    let requested = units(&source, first_leaf_range(&source));
    let (_directory, mut doc) = document(&source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let (mut request, sources) = source_request(&doc, requested.clone());
    let quota = request.batch.source_semantic_quota().unwrap();
    let before_runs = quota.remaining_runs();
    let mut job = prepare_job(&mut request, &sources, &doc);
    finish_job(&mut request, &mut job, &doc).unwrap();
    let RootLeafSemanticPublication::Ready(ready) = publish(&mut request, &mut job, &sources, &doc)
    else {
        panic!("多页真实underline结果应Ready");
    };
    assert!(ready.run_count() > PAGE_RUNS);
    assert_eq!(before_runs - quota.remaining_runs(), ready.run_count());
    assert!(quota.metadata_used() >= ready.retained_metadata());
    assert_oracle(&ready, &doc, &source, &requested);

    let (_directory, doc) = document("前言\n\n```\n正文\n```\n\n尾\n");
    let source = "前言\n\n```\n正文\n```\n\n尾\n";
    let (mut request, sources) = source_request(&doc, units(source, first_leaf_range(source)));
    let quota = request.batch.source_semantic_quota().unwrap();
    quota.admit_retained(quota.remaining_runs(), 0).unwrap(); // 与原table/来源同一个库存，不能另开Leaf额度。
    let mut job = prepare_job(&mut request, &sources, &doc);
    assert_eq!(
        finish_job(&mut request, &mut job, &doc),
        Err(MarkdownTableRequestError::Window(
            MarkdownTableWindowError::SemanticLimit
        ))
    );
    assert_eq!(job.runs.page_count(), 0); // 原quota拒绝发生在任何新run页分配前。
    assert!(matches!(
        sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap(),
        MarkdownOrdinarySemanticState::SourcesOnly
    ));
}

#[test]
fn full_selection_cancel_publication_and_root_identity_revoke_permissions() {
    let source = "前言\n\n```\n中文😀\n```\n\n尾\n";
    let requested = units(source, first_leaf_range(source));
    let (_directory, mut doc) = document(source);
    let forward = MarkdownTextSelection {
        anchor_utf16: requested.start,
        head_utf16: requested.end,
    };
    doc.set_selection(forward).unwrap();
    let (mut request, sources) = source_request(&doc, requested.clone());
    let mut job = prepare_job(&mut request, &sources, &doc);
    finish_job(&mut request, &mut job, &doc).unwrap();
    let RootLeafSemanticPublication::Ready(ready) = publish(&mut request, &mut job, &sources, &doc)
    else {
        panic!("原Root叶块应Ready");
    };
    let mut reverse = authority(&doc);
    reverse.selection = MarkdownTextSelection {
        anchor_utf16: forward.head_utf16,
        head_utf16: forward.anchor_utf16,
    };
    assert_eq!(
        ready.query_for(&doc, reverse, requested.start),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        ready.query_for(&doc, cancelled, requested.start),
        Err(MarkdownTableRequestError::Cancelled)
    );
    let (_foreign_directory, foreign) = document(source);
    assert_eq!(
        ready.query_for(&foreign, authority(&foreign), requested.start),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    drop(sources);
    assert_eq!(
        ready.query_for(&doc, authority(&doc), requested.start),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );

    let (mut request, sources) = source_request(&doc, requested);
    let mut job = prepare_job(&mut request, &sources, &doc);
    job.cancel();
    assert_eq!(
        job.advance(
            &doc,
            authority(&doc),
            true,
            4096,
            request.remaining_source_limits().unwrap(),
            &mut request.semantic
        ),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        job.advance(
            &doc,
            authority(&doc),
            true,
            65536,
            request.remaining_source_limits().unwrap(),
            &mut request.semantic
        ),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert!(job.record.is_none());
    assert!(job.underline.is_none());
    assert_eq!(job.candidates.len, 0);
    assert_eq!(job.coverage.len, 0);
    let weak_root = std::sync::Arc::downgrade(doc.root.as_ref().unwrap());
    drop(doc);
    assert!(
        weak_root.upgrade().is_none(),
        "请求、Ready和Leaf票据不得保活document/piece root/mmap"
    );
}

#[test]
fn nested_sources_remain_pending_and_cross_block_gaps_remain_unknown() {
    for source in [
        "> ```\n> 中文\n> ```\n\n尾\n",
        "- ```\n  中文\n  ```\n\n尾\n",
    ] {
        let requested = units(source, first_leaf_range(source));
        let (_directory, doc) = document(source);
        let (mut request, sources) = source_request(&doc, requested);
        let mut job = prepare_job(&mut request, &sources, &doc);
        finish_job(&mut request, &mut job, &doc).unwrap();
        let RootLeafSemanticPublication::Pending(reason) =
            publish(&mut request, &mut job, &sources, &doc)
        else {
            panic!("真实容器不能由本Root job猜样式");
        };
        assert_eq!(reason, MarkdownRootLeafPendingReason::NonRootScope);
    }
    let source = "```\n代码\n```\n\n普通正文\n";
    let (_directory, doc) = document(source);
    let (mut request, sources) = source_request(&doc, 0..doc.utf16_len());
    let mut job = prepare_job(&mut request, &sources, &doc);
    finish_job(&mut request, &mut job, &doc).unwrap();
    let RootLeafSemanticPublication::Ready(ready) = publish(&mut request, &mut job, &sources, &doc)
    else {
        panic!("跨块请求中独立已证的Root代码交集应保持可读");
    };
    let code = units(source, first_leaf_range(source));
    assert_eq!(ready.coverage_for(&doc, authority(&doc)).unwrap(), &code);
    assert_eq!(
        ready.requested_for(&doc, authority(&doc)).unwrap(),
        &(0..doc.utf16_len())
    );
    assert!(
        ready
            .query_for(&doc, authority(&doc), code.end)
            .unwrap()
            .is_none()
    );
    assert!(
        ready
            .query_for(&doc, authority(&doc), doc.utf16_len() - 1)
            .unwrap()
            .is_none()
    );
}
