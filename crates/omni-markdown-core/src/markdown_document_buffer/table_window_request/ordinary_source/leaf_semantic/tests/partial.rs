//! 部分覆盖必须与完整语义oracle相符；已证Root Link真实消费，块间separator/未知scope仍拒绝。

use super::production::{finish_sources, leaf_sources};
use super::*;

fn ready(sources: &VerifiedMarkdownViewportSources) -> &VerifiedMarkdownRootLeafViewport {
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("窗口至少含一个独立已证的Root交集");
    };
    ready
}

/// 完整parser只存在于测试；按显式预期交集逐字符比较，空洞连默认styles也不能获得。
pub(super) fn assert_partial_oracle(
    source: &str,
    document: &MarkdownDocumentBuffer,
    ready: &VerifiedMarkdownRootLeafViewport,
    requested: Range<usize>,
    expected: &[Range<usize>],
) {
    let mut baseline = MarkdownViewportPresentation::default();
    baseline.set_active_selection(ordered_selection(document.selection()));
    baseline
        .rebuild(document.generation(), 0..document.utf16_len(), source)
        .unwrap();
    // LeafBlockKind只描述原producer的Leaf来源；新接通的普通段落仍是Line来源，不应
    // 冒成代码块。分类来自独立完整Parser，不能把返回None与未覆盖的semanticGap混淆。
    let blocks: Vec<_> = Parser::new_ext(
        source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .filter_map(|(event, range)| {
        let kind = match event {
            Event::Start(Tag::Paragraph | Tag::Heading { .. }) => None,
            Event::Start(Tag::CodeBlock(pulldown_cmark::CodeBlockKind::Fenced(_))) => {
                Some(LeafBlockKind::FencedCode)
            }
            Event::Start(Tag::CodeBlock(pulldown_cmark::CodeBlockKind::Indented)) => {
                Some(LeafBlockKind::IndentedCode)
            }
            Event::Rule => Some(LeafBlockKind::ThematicBreak),
            _ => return None,
        };
        Some((units(source, range), kind))
    })
    .collect();
    let actual: Vec<_> = ready
        .covered_ranges_for(document, authority(document))
        .unwrap()
        .cloned()
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        ready.requested_for(document, authority(document)).unwrap(),
        &requested
    );
    for index in requested {
        let query = ready
            .query_for(document, authority(document), index)
            .unwrap();
        if let Some(coverage) = expected.iter().find(|range| range.contains(&index)) {
            let query = query.expect("实际Root交集必须逐字符有真实样式");
            assert_eq!(
                query.styles,
                baseline.query(index).unwrap().styles,
                "index={index}"
            );
            assert!(
                query.utf16_range.start >= coverage.start && query.utf16_range.end <= coverage.end
            );
            // quote代码的原prefix在显式golden coverage内；按相交的同一Parser块确认其
            // Leaf类别。真正separator既不在expected内，也仍须同时拒绝样式和block查询。
            let (_, expected_kind) = blocks
                .iter()
                .find(|(range, _)| range.start < coverage.end && coverage.start < range.end)
                .expect("每份显式已证coverage必须对应真实Parser块，不可由空隙造权");
            assert_eq!(
                ready
                    .block_kind_at_for(document, authority(document), index)
                    .unwrap(),
                *expected_kind,
                "index={index}"
            );
        } else {
            assert!(
                query.is_none(),
                "未覆盖的index={index}不能获得默认或邻块样式"
            );
            assert!(
                ready
                    .block_kind_at_for(document, authority(document), index)
                    .unwrap()
                    .is_none()
            );
        }
    }
}

/// closing marker后的物理separator只推进producer cursor，不属于Parser的CodeBlock语义范围。
/// 两套独立证据都必须成立：完整Parser没有给它代码样式，部分覆盖也没有授予它读取权限。
fn assert_fence_separator_is_unknown(
    source: &str,
    document: &MarkdownDocumentBuffer,
    ready: &VerifiedMarkdownRootLeafViewport,
    separator_bytes: Range<usize>,
    expected_separator: &str,
) {
    assert_eq!(&source[separator_bytes.clone()], expected_separator);
    let mut baseline = MarkdownViewportPresentation::default();
    baseline
        .rebuild(document.generation(), 0..document.utf16_len(), source)
        .unwrap();
    for index in units(source, separator_bytes) {
        assert!(
            !baseline
                .query(index)
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::FENCED_CODE)
        );
        assert!(
            ready
                .query_for(document, authority(document), index)
                .unwrap()
                .is_none()
        );
        assert!(
            ready
                .block_kind_at_for(document, authority(document), index)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn mixed_window_publishes_real_root_links_and_nested_code_without_covering_separators() {
    let code = "```rust\r\n中文😀<u>代码</u>\r\n```\r\n";
    let rule = "***\n";
    let unknown = format!(
        "**普通正文{}** [未知链接](https://example.com)",
        "中".repeat(700)
    );
    let source = format!(
        "前面的{unknown}\n\n{code}\n中间{unknown}\n\n> ```\n> 容器代码\n> ```\n\n{rule}\n尾部{unknown}\n"
    );
    let code_start = source.find(code).unwrap();
    // 显式golden止于末尾三个反引号；CRLF由consumed_end消费，不能扩张source_range。
    let code_marker_end = code_start + code.rfind("```").unwrap() + 3;
    assert_eq!(first_leaf_range(&source), code_start..code_marker_end);
    let mut expected = Vec::new();
    let mut quote_start = None;
    for (event, range) in Parser::new_ext(
        &source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    {
        match event {
            Event::Start(Tag::BlockQuote(_)) => quote_start = Some(range.start),
            Event::End(pulldown_cmark::TagEnd::BlockQuote(_)) => quote_start = None,
            Event::Start(Tag::CodeBlock(_)) => {
                // 新能力包含原 quote 首行 prefix；末端仍是原 Parser 的真正 CodeBlock 末端。
                expected.push(units(
                    &source,
                    quote_start.unwrap_or(range.start)..range.end,
                ));
            }
            Event::Start(Tag::Paragraph) => expected.push(units(&source, range)),
            Event::Rule => expected.push(units(&source, range)),
            _ => {}
        }
    }
    let (_directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    // 完整方向仍进入同一语义票据，不能按有序范围补签相反方向。
    doc.set_selection(MarkdownTextSelection {
        anchor_utf16: doc.utf16_len(),
        head_utf16: 0,
    })
    .unwrap();
    let requested = 0..doc.utf16_len();
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let ready = ready(&sources);
    // 原来被称为未知的direct link现已获原流式Parser证明；同一原字面必须逐位置迁正，
    // 而真正块间separator仍不获得邻块权限，不能把整窗笼统签成一份Ready。
    assert_partial_oracle(&source, &doc, ready, requested.clone(), &expected);
    assert_fence_separator_is_unknown(
        &source,
        &doc,
        ready,
        code_marker_end..code_start + code.len(),
        "\r\n",
    );
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::InvalidRequest)
    );
    assert_eq!(
        ready.block_kind_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::InvalidRequest)
    );
    let rule_start = source.rfind(rule).unwrap();
    assert_eq!(
        ready
            .block_kind_at_for(
                &doc,
                authority(&doc),
                units(&source, code_start..code_start).start
            )
            .unwrap(),
        Some(LeafBlockKind::FencedCode)
    );
    assert_eq!(
        ready
            .block_kind_at_for(
                &doc,
                authority(&doc),
                units(&source, rule_start..rule_start).start,
            )
            .unwrap(),
        Some(LeafBlockKind::ThematicBreak)
    );
    assert_short_link_payload_work(&request, &doc, &source, &requested, ready);
    drop((request, sources, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn viewport_edges_clip_only_true_leaf_source_and_preserve_unicode_half_unit_queries() {
    let unknown = "中".repeat(700);
    let source = format!(
        "**前言{unknown}** [未知](https://example.com)\n\n```text\n中文😀<u>强调</u>\n```\n\n**尾部{unknown}** [未知](https://example.com)\n"
    );
    let source = source.as_str();
    let emoji = source.find('😀').unwrap();
    let emoji_utf16 = source[..emoji].encode_utf16().count();
    for requested in [
        0..emoji_utf16 + 2,
        emoji_utf16..source.encode_utf16().count(),
    ] {
        let (_directory, doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let expected: Vec<_> = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::Start(Tag::Paragraph | Tag::CodeBlock(_)))
                .then(|| units(source, range))
        })
        .filter_map(|range| {
            let clipped = range.start.max(requested.start)..range.end.min(requested.end);
            (!clipped.is_empty()).then_some(clipped)
        })
        .collect();
        let (request, sources) = leaf_sources(&doc, requested.clone());
        let ready = ready(&sources);
        assert_partial_oracle(source, &doc, ready, requested.clone(), &expected);
        assert_eq!(
            ready.coverage_for(&doc, authority(&doc)),
            Err(MarkdownTableRequestError::InvalidRequest)
        );
        // NSString可查询emoji第二个UTF16单位，真实交集内的样式必须保持同一Unicode语义。
        assert_eq!(
            ready
                .query_for(&doc, authority(&doc), emoji_utf16)
                .unwrap()
                .unwrap()
                .styles,
            ready
                .query_for(&doc, authority(&doc), emoji_utf16 + 1)
                .unwrap()
                .unwrap()
                .styles
        );
        assert_short_link_payload_work(&request, &doc, source, &requested, ready);
        drop((request, sources, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn root_link_and_real_eof_code_publish_only_independently_proven_intersections() {
    let first = "```text\n首块中文\n```\n";
    let source = format!(
        "{first}\n**普通正文{}** [未知](https://example.com)\n\n````\n未闭合正文\n```",
        "中".repeat(700)
    );
    let (_directory, doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    let (request, sources) = leaf_sources(&doc, 0..doc.utf16_len());
    let ready = ready(&sources);
    let marker_end = first.rfind("```").unwrap() + 3;
    assert_eq!(first_leaf_range(&source), 0..marker_end);
    let expected = Parser::new_ext(
        &source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .filter_map(|(event, range)| {
        matches!(event, Event::Start(Tag::Paragraph | Tag::CodeBlock(_)))
            .then(|| units(&source, range))
    })
    .collect::<Vec<_>>();
    assert_partial_oracle(&source, &doc, ready, 0..doc.utf16_len(), &expected);
    assert_fence_separator_is_unknown(&source, &doc, ready, marker_end..first.len(), "\n");
    assert_short_link_payload_work(&request, &doc, &source, &(0..doc.utf16_len()), ready);
    drop((request, sources, doc));
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
}

#[test]
fn multiple_coverage_pages_share_one_global_and_forward_quota_with_live_old_publication() {
    let mut source = String::new();
    let mut expected = Vec::new();
    let mut separators = Vec::new();
    for index in 0..LEAF_PAGE_ENTRIES + 3 {
        let first = source.encode_utf16().count();
        let block = format!("```text\n第{index}块😀\n```\n");
        let marker_end = source.len() + block.rfind("```").unwrap() + 3;
        source.push_str(&block);
        expected.push(first..source[..marker_end].encode_utf16().count());
        separators.push(marker_end..source.len());
        // 短 scoped 链接已经接原 Parser；这个分页/累计配额负例保留真实 >2KiB 的长链接空洞。
        // marker goldens、两页覆盖和零复制断言都不变，不能再把新支持的小块当作应当未知。
        source.push_str(&format!(
            "\n> 间隔[{}](https://example.com)\n\n",
            "真实未知链接".repeat(128)
        ));
    }
    // golden来自显式marker端点，并独立对照完整Parser，而不是从Core覆盖反推预期。
    let parser_ranges: Vec<_> = Parser::new_ext(
        &source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .filter(|(event, _)| matches!(event, Event::Start(Tag::CodeBlock(_))))
    .map(|(_, range)| units(&source, range))
    .collect();
    assert_eq!(expected, parser_ranges);
    let (_directory, doc) = document(&source);
    let first = 0..expected[LEAF_PAGE_ENTRIES].end;
    let (mut request, previous) = leaf_sources(&doc, first.clone());
    let old = ready(&previous);
    assert_eq!(old.coverage.page_count(), 2);
    assert_partial_oracle(
        &source,
        &doc,
        old,
        first.clone(),
        &expected[..LEAF_PAGE_ENTRIES + 1],
    );
    for separator in &separators[..LEAF_PAGE_ENTRIES + 1] {
        assert_fence_separator_is_unknown(&source, &doc, old, separator.clone(), "\n");
    }
    let old_runs = old.run_count();
    let old_metadata = previous.retained_metadata;
    let global = request.batch.source_probe_state().0;
    assert_ne!(global, 0);
    let next = expected[1].start..doc.utf16_len();
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    assert_eq!(quota.remaining_runs(), 8192 - old_runs);
    assert!(quota.metadata_used() >= old_metadata);
    finish_sources(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, global);
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert_partial_oracle(&source, &doc, ready(&current), next, &expected[1..]);
    for separator in &separators[1..] {
        assert_fence_separator_is_unknown(&source, &doc, ready(&current), separator.clone(), "\n");
    }
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn partial_coverage_remains_bound_to_full_selection_original_publication_and_root_lifetime() {
    let source = "```text\n中文😀\n```\n\n普通正文\n\n***\n";
    let (_directory, mut doc) = document(source);
    let selection = MarkdownTextSelection {
        anchor_utf16: 0,
        head_utf16: doc.utf16_len(),
    };
    doc.set_selection(selection).unwrap();
    let (mut request, previous) = leaf_sources(&doc, 0..doc.utf16_len());
    let root = std::sync::Arc::downgrade(doc.root.as_ref().unwrap());
    let ready = ready(&previous);
    let mut reversed = authority(&doc);
    reversed.selection = MarkdownTextSelection {
        anchor_utf16: selection.head_utf16,
        head_utf16: selection.anchor_utf16,
    };
    assert!(matches!(
        ready.covered_ranges_for(&doc, reversed),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    doc.insert_utf16(0, "改", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert!(matches!(
        ready.covered_ranges_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert!(request.leaf_semantic.is_none());
    drop(doc);
    assert_eq!(root.strong_count(), 0, "多段来源与run页仍只能弱持原piece根");
}
