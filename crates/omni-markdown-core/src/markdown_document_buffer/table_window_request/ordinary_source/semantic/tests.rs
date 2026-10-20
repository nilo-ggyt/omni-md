//! 完整旧投影仅作为有限合成夹具的独立样式 oracle，不进入产品请求。

use super::*;
use crate::markdown_viewport_presentation::MarkdownViewportPresentation;
use pulldown_cmark::{Event, Parser, Tag};

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("有限普通语义验收.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}

fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
    MarkdownViewportSourceAuthority {
        context_revision: 41,
        selection: document.selection(),
        cancelled: false,
    }
}

/// 测试只推进原单端点，不启用同片续用；真实读入字节作为联合请求减少重复读取的独立对照。
fn finish_position(document: &MarkdownDocumentBuffer, locator: &mut PositionLocator) -> usize {
    let mut source_bytes = 0;
    for _ in 0..100 {
        if locator.result().is_some() {
            return source_bytes;
        }
        let work = locator.advance(document, 4096).unwrap();
        assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        source_bytes += work.source_bytes;
    }
    panic!("独立小片端点未在有界对照内完成");
}

#[test]
/// 同一OriginalSpan节点仍有多个真实ordinal；附近不同片和远处同ordinal都不能继承原前缀。
fn position_prefix_reuse_requires_original_leaf_ordinal_and_logical_coordinates() {
    let (_directory, doc) = document(&"a".repeat(100_000));
    let mut first = PositionLocator::new(161, false);
    assert_eq!(finish_position(&doc, &mut first), 161);
    let mut same = PositionLocator::new(501, true);
    same.advance(&doc, 4096).unwrap();
    assert!(same.reuse_completed_prefix(&first));
    assert_eq!(finish_position(&doc, &mut same), 340);
    assert_eq!(same.result(), Some(501));

    let mut other = PositionLocator::new(5000, true);
    other.advance(&doc, 4096).unwrap();
    assert_eq!(
        first.leaf.as_ref().unwrap().node,
        other.leaf.as_ref().unwrap().node
    );
    assert_ne!(
        first.leaf.as_ref().unwrap().ordinal,
        other.leaf.as_ref().unwrap().ordinal
    );
    assert!(!other.reuse_completed_prefix(&first));
    assert_eq!(finish_position(&doc, &mut other), 5000 - 4096);
    assert_eq!(other.result(), Some(5000));

    let mut far = PositionLocator::new(65536 + 501, true);
    far.advance(&doc, 4096).unwrap();
    assert_eq!(
        first.leaf.as_ref().unwrap().ordinal,
        far.leaf.as_ref().unwrap().ordinal
    );
    assert_ne!(
        first.leaf.as_ref().unwrap().node,
        far.leaf.as_ref().unwrap().node
    );
    assert!(!far.reuse_completed_prefix(&first));
    assert_eq!(finish_position(&doc, &mut far), 501);
    assert_eq!(far.result(), Some(65536 + 501));
}

#[test]
/// 半代理项的floor结果不等于读到的scalar尾，ceil也不能把目标仍在scalar内的另一票提升到尾端。
fn position_prefix_reuse_preserves_non_bmp_floor_ceil_and_incomplete_scalar() {
    let (_directory, doc) = document("😀z\n");
    let mut floor = PositionLocator::new(1, false);
    assert_eq!(finish_position(&doc, &mut floor), 4);
    assert_eq!(floor.result(), Some(0));
    let mut end = PositionLocator::new(2, true);
    end.advance(&doc, 4096).unwrap();
    assert!(!end.reuse_completed_prefix(&floor));
    assert_eq!(finish_position(&doc, &mut end), 4);
    assert_eq!(end.result(), Some(4));

    let mut ceil = PositionLocator::new(1, true);
    assert_eq!(finish_position(&doc, &mut ceil), 4);
    let mut inside = PositionLocator::new(1, true);
    inside.advance(&doc, 4096).unwrap();
    assert!(!inside.reuse_completed_prefix(&ceil));
    assert_eq!(finish_position(&doc, &mut inside), 4);
    assert_eq!(inside.result(), Some(4));
    let mut exact = PositionLocator::new(2, true);
    exact.advance(&doc, 4096).unwrap();
    assert!(exact.reuse_completed_prefix(&ceil));
    assert_eq!(finish_position(&doc, &mut exact), 0);
    assert_eq!(exact.result(), Some(4));

    let mut partial = PositionLocator::new(1, true);
    partial.advance(&doc, 4096).unwrap();
    partial.advance(&doc, partial.minimum_budget(&doc)).unwrap();
    assert_ne!(partial.remaining, 0);
    let mut target = PositionLocator::new(2, true);
    target.advance(&doc, 4096).unwrap();
    assert!(!target.reuse_completed_prefix(&partial));
    assert_eq!(finish_position(&doc, &mut target), 4);
    assert_eq!(target.result(), Some(4));
}

fn finish(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    // 最小公开4096量子必须真实推进；每个实际来源/转换/输出步骤仍受单步硬上限约束。
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(document, authority(document), 4096)?;
        assert!(work.source_bytes <= 4096);
        assert!(work.structural_transitions <= 4096);
        assert!(work.metadata_bytes <= 4096);
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("有限普通语义没有在真实累计预算内结束");
}

#[test]
fn root_plain_finite_query_matches_complete_projection() {
    let source = "普通中文😀第一行\r\n第二行没有特殊符号\r\n";
    let (_directory, doc) = document(source);
    let requested = 2..doc.utf16_len() - 2;
    let mut request = doc
        .viewport_plain_source_request(requested.clone(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request.sources_for(&doc, authority(&doc)).unwrap().unwrap();
    let MarkdownOrdinarySemanticState::PlainReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("真实 Root 纯文本必须完成有限语义");
    };
    let mut baseline = MarkdownViewportPresentation::default();
    baseline.set_active_selection(ordered_selection(doc.selection()));
    baseline
        .rebuild(doc.generation(), 0..doc.utf16_len(), source)
        .unwrap();
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)).unwrap(),
        &requested
    );
    for index in requested.clone() {
        // 完整 pinned Parser 的旧投影给出期望值，不能由新 runs 反推自身正确性。
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles,
            baseline.query(index).unwrap().styles
        );
    }
    assert!(
        ready
            .query_for(&doc, authority(&doc), requested.start - 1)
            .unwrap()
            .is_none()
    );
    assert!(
        ready
            .query_for(&doc, authority(&doc), requested.end)
            .unwrap()
            .is_none()
    );
    let total = request.total_work();
    assert_eq!(total.copied_source_bytes, 0);
    assert_eq!(total.parser_input_bytes, 0);
    assert_eq!(total.allocated_source_capacity, 0);
}

#[test]
fn setext_marker_crlf_and_real_global_underline_match_complete_projection() {
    for source in [
        "   中文标题😀  \r\n  ===  \r\n\r\n另一段\n",
        "中文标题\r\n  ---\t\r\n\r\n另一段\n",
        "<u>前面的省略块\n\n标题😀  \r\n===\r\n\r\n后面的省略块</u>\n",
        "<u>前面的省略块\n\n```text\n</u>\n```\n\n标题😀\n===\n\n后面的省略块</u>\n",
        "<u>前面的省略块\n\n    </u>\n\n标题😀\n===\n\n后面的省略块</u>\n",
    ] {
        let options =
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true);
        let heading = Parser::new_ext(source, options)
            .into_offset_iter()
            .find_map(|(event, range)| {
                matches!(event, Event::Start(Tag::Heading { .. })).then_some(range)
            })
            .unwrap();
        let requested = source[..heading.start].encode_utf16().count()
            ..source[..heading.end].encode_utf16().count();
        for caret in [requested.start, source.encode_utf16().count()] {
            // 两个真实 Core 选区分别覆盖 active/inactive owner；不只检查样式位存在。
            let (_directory, mut doc) = document(source);
            doc.set_selection(MarkdownTextSelection::caret(caret))
                .unwrap();
            let mut request = doc
                .viewport_plain_source_request(requested.clone(), 41, doc.selection())
                .unwrap();
            finish(&mut request, &doc).unwrap();
            let sources = request.sources_for(&doc, authority(&doc)).unwrap().unwrap();
            let MarkdownOrdinarySemanticState::PlainReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实 Root Setext 应有有限 heading/marker 语义");
            };
            let mut baseline = MarkdownViewportPresentation::default();
            baseline.set_active_selection(ordered_selection(doc.selection()));
            baseline
                .rebuild(doc.generation(), 0..doc.utf16_len(), source)
                .unwrap();
            for index in requested.clone() {
                // 远处 <u> 配对与 Setext owner 都来自完整原源；省略段不能重设其上下文。
                assert_eq!(
                    ready
                        .query_for(&doc, authority(&doc), index)
                        .unwrap()
                        .unwrap()
                        .styles,
                    baseline.query(index).unwrap().styles,
                    "原源={source:?}, caret={caret}, index={index}"
                );
            }
            assert!(
                ready
                    .runs_for(&doc, authority(&doc))
                    .unwrap()
                    .any(|run| run.styles.contains(MarkdownPresentationStyles::MARKER))
            );
        }
    }
}

#[test]
fn giant_unicode_line_proves_only_actual_finite_byte_coverage() {
    let source = format!("{}\r\n", "中".repeat(100_000));
    let (_directory, doc) = document(&source);
    let requested = 33_333..33_350;
    let mut request = doc
        .viewport_plain_source_request(requested.clone(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let semantic = request.semantic.as_ref().unwrap();
    // 独立按固定三字节中文 scalar 计算边界；300KiB 原行不能被整块交给256KiB上下文入口。
    assert_eq!(
        semantic.byte_coverage,
        Some(requested.start * 3..requested.end * 3)
    );
    assert_eq!(
        semantic.proof.as_ref().unwrap().proven_end(),
        requested.end * 3
    );
    assert!(semantic.proof.as_ref().unwrap().terminal().is_none());
    let sources = request.sources_for(&doc, authority(&doc)).unwrap().unwrap();
    let MarkdownOrdinarySemanticState::PlainReady(ready) = sources
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("巨型原行的有限纯文本区间必须可查");
    };
    for index in requested.clone() {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles,
            MarkdownPresentationStyles::default()
        );
    }
    assert_eq!(
        ready.block_source_range_for(&doc, authority(&doc)).unwrap(),
        0..source.len()
    );
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
}

#[test]
fn forward_prefix_keeps_original_publications_and_streams_later_direct_link() {
    let mut source = (0..120)
        .map(|line| format!("第{line}行中😀\r\n"))
        .collect::<String>();
    source.push_str("后面的 [复杂语法](https://example.com/not-yet-streamed)\r\n");
    let starts = source
        .match_indices('第')
        .map(|(byte, _)| byte)
        .collect::<Vec<_>>();
    let units = |byte| source[..byte].encode_utf16().count();
    let (directory, doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let mut request = doc
        .viewport_plain_source_request(units(starts[20])..units(starts[25]), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let first = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let global = request.batch.source_probe_state().0;
    let origin = match request.job.as_ref().unwrap() {
        OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
        _ => panic!("夹具只有一个真实paragraph"),
    };
    let before = request
        .semantic
        .as_ref()
        .unwrap()
        .cursor
        .as_ref()
        .unwrap()
        .total_work();
    assert!(
        request.semantic.as_ref().unwrap().underline.is_none(),
        "真实发布结束先释放临时配对库存，不能跨下一 quota 延迟释放"
    );
    let next = units(starts[23])..units(starts[31]);
    request
        .continue_forward(&doc, &first, next.clone(), authority(&doc))
        .unwrap();
    assert!(
        request.batch.source_probe_state().2
            >= first.retained_metadata
                + source_workspace_metadata()
                + publication_reservation_metadata()
    );
    finish(&mut request, &doc).unwrap();
    let after = request
        .semantic
        .as_ref()
        .unwrap()
        .cursor
        .as_ref()
        .unwrap()
        .total_work();
    // 真实 child 累计读取只增加原前缀到新末端的差；固定内存地址相同不足以证明未重建游标。
    assert_eq!(
        after.byte_reads - before.byte_reads,
        starts[31] - starts[25]
    );
    assert!(after.transitions > before.transitions);
    assert_eq!(request.batch.source_probe_state().0, global);
    assert_eq!(
        match request.job.as_ref().unwrap() {
            OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
            _ => 0,
        },
        origin
    );
    assert!(Rc::ptr_eq(
        &first.blocks[0],
        request.semantic.as_ref().unwrap().block.as_ref().unwrap()
    ));
    let second = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    request
        .continue_forward(
            &doc,
            &second,
            units(starts[110])..doc.utf16_len(),
            authority(&doc),
        )
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let third = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::PlainReady(ready) =
        third.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
    else {
        panic!("后缀的真实直接链接已接流式语法，不得保留旧General断言");
    };
    let requested = units(starts[110])..doc.utf16_len();
    let expected_owner = Parser::new_ext(
        &source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| match event {
        Event::Start(Tag::Link { dest_url, .. }) => Some((range, dest_url.into_string())),
        _ => None,
    })
    .unwrap();
    let label = Parser::new_ext(
        &source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| match event {
        Event::Text(text) if text.as_ref() == "复杂语法" => {
            Some(units(range.start)..units(range.end))
        }
        _ => None,
    })
    .unwrap();
    let mut baseline = MarkdownViewportPresentation::default();
    baseline.set_active_selection(ordered_selection(doc.selection()));
    baseline
        .rebuild(doc.generation(), 0..doc.utf16_len(), &source)
        .unwrap();
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)).unwrap(),
        &requested
    );
    assert_eq!(
        ready.block_source_range_for(&doc, authority(&doc)).unwrap(),
        0..source.len()
    );
    // 从完整Parser取得owner和真实child label；新pub逐位置保护marker/目标显隐与点击权限。
    // 首两批旧pub仍只对自己原覆盖有效，不能因同源后缀Ready而扩大旧票权限。
    for index in requested.clone() {
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles,
            baseline.query(index).unwrap().styles,
            "index={index}"
        );
        let link = ready.link_at_for(&doc, authority(&doc), index).unwrap();
        if label.contains(&index) {
            let link = link.unwrap();
            assert_eq!(link.label_utf16_range, label);
            assert_eq!(link.destination.clone().into_string(), expected_owner.1);
        } else {
            assert!(link.is_none());
        }
    }
    let owner_start = units(expected_owner.0.start);
    assert!(
        ready
            .query_for(&doc, authority(&doc), owner_start)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::MARKER)
    );
    let MarkdownOrdinarySemanticState::PlainReady(second_ready) = second
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("原中间pub仍须有效");
    };
    assert!(matches!(
        second_ready.link_at_for(&doc, authority(&doc), label.start),
        Err(MarkdownTableRequestError::Window(
            MarkdownTableWindowError::ContextPending
        ))
    ));
    assert!(matches!(
        request.continue_forward(&doc, &first, requested.clone(), authority(&doc)),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)).unwrap(),
        &requested
    );
    let MarkdownOrdinarySemanticState::PlainReady(old) =
        first.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
    else {
        panic!("旧有限证明必须保留");
    };
    assert_eq!(
        old.query_for(&doc, authority(&doc), units(starts[20]))
            .unwrap()
            .unwrap()
            .styles,
        MarkdownPresentationStyles::default()
    );
    assert_eq!(doc.generation(), 0);
    assert!(!doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有限普通语义验收.md")).unwrap(),
        source.as_bytes()
    );
    // source/pub只保Weak及不可变URI；即使三份pub与请求仍活，真正Doc退休也必须释放mmap。
    drop(doc);
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
    drop((first, second, third, request));
}

#[test]
fn root_links_references_and_images_match_parser_while_cross_block_holes_stay_pending() {
    // 保留原长段落字面；过去的直接链接Pending已过期，必须保护真实owner/label/选择显隐。
    // 引用Missing与Image已沿同Global/唯一文件接入，下方保原字面改验真实Parser结果。
    let complex = format!(
        "普通 **{}** [未决链接](https://example.com)\n",
        "粗体".repeat(400)
    );
    let (owner, target) = Parser::new_ext(
        &complex,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| match event {
        Event::Start(Tag::Link { dest_url, .. }) => Some((range, dest_url.into_string())),
        _ => None,
    })
    .unwrap();
    let units = |byte| complex[..byte].encode_utf16().count();
    let label = Parser::new_ext(
        &complex,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| match event {
        Event::Text(text) if text.as_ref() == "未决链接" => {
            Some(units(range.start)..units(range.end))
        }
        _ => None,
    })
    .unwrap();
    for selection in [
        MarkdownTextSelection::caret(0),
        MarkdownTextSelection::caret(label.start + 1),
        MarkdownTextSelection {
            anchor_utf16: label.end,
            head_utf16: label.start,
        },
    ] {
        let (directory, mut doc) = document(&complex);
        let backing = doc.original_backing_observer().unwrap();
        doc.set_selection(selection).unwrap();
        let mut request = doc
            .viewport_plain_source_request(0..doc.utf16_len(), 41, selection)
            .unwrap();
        finish(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::PlainReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原合法direct语法必须产生完整真实语义");
        };
        let mut baseline = MarkdownViewportPresentation::default();
        baseline.set_active_selection(ordered_selection(selection));
        baseline
            .rebuild(doc.generation(), 0..doc.utf16_len(), &complex)
            .unwrap();
        assert_eq!(
            ready.block_source_range_for(&doc, authority(&doc)).unwrap(),
            0..complex.len()
        );
        for index in 0..doc.utf16_len() {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                baseline.query(index).unwrap().styles,
                "index={index}, selection={selection:?}"
            );
            let actual = ready.link_at_for(&doc, authority(&doc), index).unwrap();
            if label.contains(&index) {
                let actual = actual.unwrap();
                assert_eq!(actual.label_utf16_range, label);
                assert_eq!(actual.destination.clone().into_string(), target);
            } else {
                assert!(actual.is_none());
            }
        }
        // owner包括完整[]和目标，只有真实label可激活；标记显隐仍由原完整selection控制。
        let marker = ready
            .query_for(&doc, authority(&doc), units(owner.start))
            .unwrap()
            .unwrap();
        assert!(marker.styles.contains(MarkdownPresentationStyles::MARKER));
        assert_eq!(
            marker
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            selection.head_utf16 == 0
        );
        let mut revoked = authority(&doc);
        revoked.context_revision += 1;
        assert!(matches!(
            ready.link_at_for(&doc, revoked, label.start),
            Err(MarkdownTableRequestError::PublicationSuperseded)
        ));
        assert_eq!(doc.selection(), selection);
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有限普通语义验收.md")).unwrap(),
            complex.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
    let unknown_reference = format!("{} [未知引用][T]\n", "普通正文 ".repeat(400));
    let unknown_image = format!(
        "{} ![图片](https://example.com/image)\n",
        "普通正文 ".repeat(400)
    );
    for source in [unknown_reference.as_str(), unknown_image.as_str()] {
        // 原未知引用在真实EOF后仍是字面；Image保LINK/marker样式但不取得点击URI。
        // 不能仅把旧Pending删掉：完整每个位置、实际目标、磁盘/历史和释放都须成立。
        let (directory, doc) = document(source);
        let observer = doc.original_backing_observer().unwrap();
        let mut request = doc
            .viewport_plain_source_request(0..doc.utf16_len(), 41, doc.selection())
            .unwrap();
        finish(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::PlainReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原Root引用/图片字面必须沿真实端口完成，不得保留过期General断言");
        };
        let mut baseline = MarkdownViewportPresentation::default();
        baseline.set_active_selection(ordered_selection(doc.selection()));
        baseline
            .rebuild(doc.generation(), 0..doc.utf16_len(), source)
            .unwrap();
        assert!(
            !Parser::new_ext(
                source,
                crate::markdown_viewport_presentation::parser::markdown_presentation_options(true)
            )
            .any(|event| matches!(event, Event::Start(Tag::Link { .. })))
        );
        for index in 0..doc.utf16_len() {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                baseline.query(index).unwrap().styles,
                "index={index}"
            );
            assert!(
                ready
                    .link_at_for(&doc, authority(&doc), index)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有限普通语义验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
    {
        let source = "第一段\n\n第二段\n";
        let expected = MarkdownOrdinaryPendingReason::IncompleteCoverage;
        let (_directory, doc) = document(source);
        let mut request = doc
            .viewport_plain_source_request(0..doc.utf16_len(), 41, doc.selection())
            .unwrap();
        finish(&mut request, &doc).unwrap();
        let sources = request.sources_for(&doc, authority(&doc)).unwrap().unwrap();
        assert!(
            matches!(sources.ordinary_semantics_for(&doc, authority(&doc)).unwrap(), MarkdownOrdinarySemanticState::Pending(actual) if *actual == expected),
            "不能给 {source:?} 签错误的有限默认样式"
        );
        // Pending 的实际库存不包含从未分配的 Plain Box，最坏预留不会冒充实际metadata。
        let expected_metadata = sources.records.len() * record_metadata()
            + publication_metadata()
            + sources.blocks.len() * block_metadata()
            + sources.blocks.capacity() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>();
        assert_eq!(sources.retained_metadata, expected_metadata);
    }
    // 引用/列表短块已经接同源 context；保留原夹具，改为与独立完整 Parser 逐位置验证真实新行为。
    // 未覆盖洞仍断言Pending；Root引用/Image的Ready不会给任意跨块范围补权。
    for source in ["> 引用中文\n> 第二行\n", "- 列表中文\n  第二行\n"] {
        let (directory, doc) = document(source);
        let observer = doc.original_backing_observer().unwrap();
        let mut request = doc
            .viewport_plain_source_request(0..doc.utf16_len(), 41, doc.selection())
            .unwrap();
        finish(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::PlainReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("真实短容器必须完成，不能保留过期 NonRootScope 负断言")
        };
        let mut baseline = MarkdownViewportPresentation::default();
        baseline.set_active_selection(ordered_selection(doc.selection()));
        baseline
            .rebuild(doc.generation(), 0..doc.utf16_len(), source)
            .unwrap();
        assert_eq!(
            ready.coverage_for(&doc, authority(&doc)).unwrap(),
            &(0..doc.utf16_len())
        );
        for index in 0..doc.utf16_len() {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                baseline.query(index).unwrap().styles,
                "source={source:?}, index={index}"
            );
        }
        assert_eq!(
            std::fs::read(directory.path().join("有限普通语义验收.md")).unwrap(),
            source.as_bytes()
        );
        drop(sources);
        drop(request);
        drop(doc);
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
    let (_directory, doc) = document("原来源入口\n");
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    assert!(matches!(
        request
            .sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap()
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap(),
        MarkdownOrdinarySemanticState::SourcesOnly
    ));
}

#[test]
fn mutable_authority_failures_are_sticky_but_old_plain_metadata_does_not_own_source() {
    for failure in 0..3 {
        let (_directory, mut doc) = document("中文😀纯文本第一行\n第二行\n第三行\n");
        // “中/文/😀”的 UTF-16 末端依次为1/2/4；选择完整 emoji，避免在权限验收前被代理对边界校验拒绝。
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 1,
            head_utf16: 4,
        })
        .unwrap();
        let root = Arc::downgrade(doc.root.as_ref().unwrap());
        let mut request = doc
            .viewport_plain_source_request(0..10, 41, doc.selection())
            .unwrap();
        finish(&mut request, &doc).unwrap();
        let previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let mut rejected = authority(&doc);
        let expected = match failure {
            0 => {
                rejected.cancelled = true;
                MarkdownTableRequestError::Cancelled
            }
            1 => {
                rejected.context_revision += 1;
                MarkdownTableRequestError::PublicationSuperseded
            }
            _ => {
                // 有序范围相同的反向选择仍是另一完整上下文，不能替原请求续权。
                rejected.selection = MarkdownTextSelection {
                    anchor_utf16: doc.selection().head_utf16,
                    head_utf16: doc.selection().anchor_utf16,
                };
                assert_eq!(
                    ordered_selection(rejected.selection),
                    ordered_selection(doc.selection())
                );
                assert_ne!(rejected.selection, doc.selection());
                MarkdownTableRequestError::PublicationSuperseded
            }
        };
        assert_eq!(
            request
                .continue_forward(&doc, &previous, 2..12, rejected)
                .err(),
            Some(expected)
        );
        assert!(
            request.semantic.is_none()
                && request.blocks.is_empty()
                && request.records.is_empty()
                && request.job.is_none()
                && request.coordinates.is_none()
                && request.ready.is_none()
        );
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(expected)
        );
        let MarkdownOrdinarySemanticState::PlainReady(old) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("独立旧metadata仍可读");
        };
        assert!(old.query_for(&doc, authority(&doc), 2).unwrap().is_some());
        drop(request);
        drop(doc);
        assert_eq!(
            root.strong_count(),
            0,
            "外带 seed/runs 不得强持原 piece root 或映射"
        );
    }
}

#[test]
fn edit_undo_same_root_and_other_document_revoke_true_plain_publication() {
    let source = "中文😀纯文本\r\n第二行\r\n";
    let (_directory, mut doc) = document(source);
    let (_other_directory, other) = document(source);
    let root = Arc::downgrade(doc.root.as_ref().unwrap());
    let generation = doc.generation();
    let mut request = doc
        .viewport_plain_source_request(0..10, 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::PlainReady(ready) = previous
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("夹具必须完成真实Plain");
    };
    assert!(matches!(
        ready.query_for(&other, authority(&other), 1),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    doc.insert_utf16(0, "插入", MarkdownTextSelection::caret(2))
        .unwrap();
    doc.undo().unwrap();
    assert!(std::sync::Weak::ptr_eq(
        &root,
        &Arc::downgrade(doc.root.as_ref().unwrap())
    ));
    assert!(doc.generation() > generation);
    assert!(matches!(
        ready.query_for(&doc, authority(&doc), 1),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert_eq!(
        request.take_sources_for(&doc, authority(&doc)).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert!(request.semantic.is_none() && request.blocks.is_empty());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
}

#[test]
fn previous_plain_runs_and_real_box_payload_share_new_batch_inventory_before_allocation() {
    let source = (0..30)
        .map(|line| format!("普通{line}行\n"))
        .collect::<String>();
    let (_directory, doc) = document(&source);
    let mut request = doc
        .viewport_plain_source_request(0..20, 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::PlainReady(old) = previous
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("夹具必须有真实有限run");
    };
    let old_runs = old.run_count();
    assert!(old_runs > 0);
    let actual_metadata = previous.records.len() * record_metadata()
        + publication_metadata()
        + plain_publication_metadata()
        + previous.blocks.len() * block_metadata()
        + previous.blocks.capacity() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>();
    assert_eq!(previous.retained_metadata, actual_metadata);
    request
        .continue_forward(&doc, &previous, 10..30, authority(&doc))
        .unwrap();
    assert!(
        request.batch.source_probe_state().2
            >= previous.retained_metadata
                + source_workspace_metadata()
                + publication_reservation_metadata()
    );
    // 通过同一个真实配额入口预留其余同活使用者的run槽；新Plain一段也不能绕过旧结果占用。
    request.batch.admit_source_runs(8192 - old_runs).unwrap();
    assert_eq!(
        finish(&mut request, &doc).err(),
        Some(MarkdownTableRequestError::Window(
            MarkdownTableWindowError::SemanticLimit
        ))
    );
    assert!(request.semantic.is_none() && request.blocks.is_empty() && request.ready.is_none());
    assert!(
        old.query_for(&doc, authority(&doc), 1).unwrap().is_some(),
        "联合超限只撤当前owner，不损坏旧不可变结果"
    );
}
