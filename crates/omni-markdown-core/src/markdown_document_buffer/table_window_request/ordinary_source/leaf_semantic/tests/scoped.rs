//! Publish original Global/Origin container lines through the public SourceRequest, without
//! constructing scopes, private jobs, or artificial completion tickets.

use super::production::{finish_sources, leaf_sources};
use super::*;

#[test]
fn production_scoped_plain_origins_preserve_content_markers_and_canonical_file() {
    for (source, contents, inherited) in [
        (
            "> 中文首行😀\r\n> 第二行正文\r\n",
            ["中文首行😀", "第二行正文", ""],
            MarkdownPresentationStyles::BLOCK_QUOTE,
        ),
        (
            "> - 中文条目😀\n>   后续正文\n> - 另一个条目\n",
            ["中文条目😀", "后续正文", "另一个条目"],
            MarkdownPresentationStyles::BLOCK_QUOTE
                .union(MarkdownPresentationStyles::UNORDERED_LIST),
        ),
        (
            "1. 中文有序\n   后续正文\n2. 尾项\n",
            ["中文有序", "后续正文", "尾项"],
            MarkdownPresentationStyles::ORDERED_LIST,
        ),
        (
            "- 父项\n  - 子项\n    子项续行\n",
            ["父项", "子项", "子项续行"],
            MarkdownPresentationStyles::UNORDERED_LIST,
        ),
        (
            "> 开头\n懒续行中文\n",
            ["开头", "懒续行中文", ""],
            MarkdownPresentationStyles::BLOCK_QUOTE,
        ),
        (
            "- $中文金额😀$和花括号{}\n",
            ["$中文金额😀$和花括号{}", "", ""],
            MarkdownPresentationStyles::UNORDERED_LIST,
        ),
    ] {
        let (directory, mut doc) = document(source);
        // Keep selection at document end so active-item literal visibility is not mistaken for
        // another parsing source.
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let observer = doc.original_backing_observer().unwrap();
        let (request, sources) = leaf_sources(&doc, 0..doc.utf16_len());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("真实容器纯文本必须发布当前原行的语义交集：{source:?}");
        };
        let mut baseline = MarkdownViewportPresentation::default();
        baseline.set_active_selection(ordered_selection(doc.selection()));
        baseline
            .rebuild(doc.generation(), 0..doc.utf16_len(), source)
            .unwrap();
        for content in contents.into_iter().filter(|text| !text.is_empty()) {
            let start = source.find(content).unwrap();
            for index in units(source, start..start + content.len()) {
                let actual = ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .expect("原 Origin 正文不能留下未知空洞");
                assert!(actual.styles.contains(inherited));
                assert_eq!(actual.styles, baseline.query(index).unwrap().styles);
            }
        }
        // Check structural flags only on public Origin prefixes; an empty lazy-continuation prefix
        // must not gain invented markers.
        for line in sources.ordinary_lines_for(&doc, authority(&doc)).unwrap() {
            for index in line.prefix_utf16_range() {
                let actual = ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .expect("已认证 prefix 必须随原行发布")
                    .styles;
                let expected = baseline.query(index).unwrap().styles;
                for flag in [
                    MarkdownPresentationStyles::MARKER,
                    MarkdownPresentationStyles::HIDDEN_MARKER,
                ] {
                    assert_eq!(
                        actual.contains(flag),
                        expected.contains(flag),
                        "真实prefix标记不能扩大到缩进：source={source:?} index={index}"
                    );
                }
            }
        }
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        drop(sources);
        drop(request);
        drop(doc);
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
}

#[test]
fn production_scoped_plain_uses_same_request_for_long_viewports_and_rejects_foreign_or_stale() {
    let source = format!("> {}\r\n> 真实尾行😀\r\n", "中".repeat(700_000));
    let (directory, mut doc) = document(&source);
    let observer = doc.original_backing_observer().unwrap();
    let first = 13..77;
    let far = 699_900..699_964;
    let (mut request, mut sources) = leaf_sources(&doc, first.clone());
    let initial_work = request.total_work();
    for window in [far, first] {
        if window.start < sources.requested_for(&doc, authority(&doc)).unwrap().start {
            request
                .reposition(&doc, &sources, window.clone(), authority(&doc))
                .unwrap();
        } else {
            request
                .continue_forward(&doc, &sources, window.clone(), authority(&doc))
                .unwrap();
        }
        finish_sources(&mut request, &doc).unwrap();
        sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一长容器的原 SourceRequest 必须发布真实后继或回退窗口");
        };
        for index in window {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::BLOCK_QUOTE
            );
        }
    }
    let measured = request.total_work();
    assert!(measured.source_bytes > initial_work.source_bytes);
    assert_eq!(measured.copied_source_bytes, 0);
    assert_eq!(measured.parser_input_bytes, 0);
    assert_eq!(measured.allocated_source_capacity, 0);

    let (_other_directory, other) = document(&source);
    assert!(matches!(
        sources.ordinary_semantics_for(&other, authority(&other)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    let before = authority(&doc);
    doc.set_selection(MarkdownTextSelection::caret(14)).unwrap();
    assert!(matches!(
        sources.ordinary_semantics_for(&doc, before),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_scoped_long_links_and_other_roles_keep_real_unknown_intersections() {
    for source in [
        format!("> [{}](https://example.com)\n", "长链接正文".repeat(180)),
        format!(
            "[^脚注]: [{}](https://example.com)\n",
            "长链接正文".repeat(180)
        ),
    ] {
        let (_directory, doc) = document(&source);
        let (_request, sources) = leaf_sources(&doc, 0..doc.utf16_len());
        assert!(matches!(
            sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap(),
            MarkdownOrdinarySemanticState::LeafPending(
                MarkdownRootLeafPendingReason::NeedsGeneralInline
                    | MarkdownRootLeafPendingReason::NonRootScope
            )
        )); // Long rich text cannot enter the finite whole-slice parser; a footnote opening cannot prove unfinished link semantics.
    }
}

#[test]
fn production_scoped_code_and_emphasis_cross_real_prefixes_match_original_parser() {
    for source in [
        "> **中文粗体😀\r\n> 续行** 与 *斜体*\r\n",
        "> - **嵌套\n>   列表** 与 `代码😀`\n",
        "1. **有序\r   续行** 与 *斜体*\r",
        "> **引文\n懒续行中文**\n",
        "- **星号\n\t续行😀** 与 `短码`\n",
        "> `未配对 **真实粗体\n> 跨行** 后面 ``真正代码\n> 中文😀`` 结尾\n",
        "> ` 代码\r\n> \t跨行😀 ` 后面 **正文**\r\n",
        "> ` \n> \t ` 正文\n",
        "> - ` \r\n>   \t ` 后文\r\n",
        // These fixed sources also serve Native geometry regression. Independently establish body
        // scalar/prefix semantics here so later font-shaping failure is not misdiagnosed as missing
        // SourceRequest semantics.
        "outside\n\n> 中文引用段落保留真实字体与缩进，混合 emoji 😀 和 English words，在原可用列宽内自然换行。中文继续验证实际软行来源、基线、上下边距和段落边框的占位。\n\nend\n",
        "outside\r\n\r\n> **引用中文粗体\r\n> 续行粗体** 与 *引用斜体*、`引用代码\r\n> 跨行代码😀` 和普通中文保持同一实际容器来源。\r\n\r\nend\r\n",
        "outside\n\n> - **嵌套列表中文\n>   续行粗体** 与 ` 代码\n>   \t跨行代码😀 ` 后面普通中文。\n\nend\n",
    ] {
        let (directory, mut doc) = document(source);
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let observer = doc.original_backing_observer().unwrap();
        let (request, sources) = leaf_sources(&doc, 0..doc.utf16_len());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("真实容器强调/code 必须沿同一 Origin 发布：{source:?}");
        };
        let mut oracle = MarkdownViewportPresentation::default();
        oracle.set_active_selection(ordered_selection(doc.selection()));
        oracle
            .rebuild(doc.generation(), 0..doc.utf16_len(), source)
            .unwrap();
        // Compare body scalars actually emitted by the independent parser, without deriving expected
        // values from the source runs under test.
        for (event, range) in Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        {
            if !matches!(event, Event::Text(_) | Event::Code(_)) {
                continue;
            }
            for index in units(source, range) {
                let expected = oracle.query(index).unwrap().styles;
                let actual = ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .expect("真实容器正文不得留未知空洞")
                    .styles;
                assert_eq!(actual, expected, "source={source:?} index={index}");
            }
        }
        for line in sources.ordinary_lines_for(&doc, authority(&doc)).unwrap() {
            for index in line.prefix_utf16_range() {
                let actual = ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles;
                let expected = oracle.query(index).unwrap().styles;
                for flag in [
                    MarkdownPresentationStyles::MARKER,
                    MarkdownPresentationStyles::HIDDEN_MARKER,
                ] {
                    assert_eq!(
                        actual.contains(flag),
                        expected.contains(flag),
                        "真实prefix必须保原marker显隐：source={source:?} index={index}"
                    );
                }
            }
        }
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop(sources);
        drop(request);
        drop(doc);
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
}

#[test]
fn production_scoped_giant_cross_line_inline_repositions_on_original_request_without_copy() {
    let source = format!(
        "> **{}\r\n> 真正尾端😀** 与 ``代码\r\n> 真实尾码``\r\n",
        "中".repeat(700_000)
    );
    let (directory, mut doc) = document(&source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let observer = doc.original_backing_observer().unwrap();
    let first = 20..70;
    let tail_start = source[..source.find("真正尾端").unwrap()]
        .encode_utf16()
        .count();
    let tail = tail_start..tail_start + "真正尾端😀".encode_utf16().count();
    let (mut request, mut sources) = leaf_sources(&doc, first.clone());
    let before = request.total_work();
    for window in [tail, first] {
        let current = sources.requested_for(&doc, authority(&doc)).unwrap();
        if window.start < current.start {
            request
                .reposition(&doc, &sources, window.clone(), authority(&doc))
                .unwrap();
        } else if window != *current {
            request
                .continue_forward(&doc, &sources, window.clone(), authority(&doc))
                .unwrap();
        }
        finish_sources(&mut request, &doc).unwrap();
        if let Some(next) = request.take_sources_for(&doc, authority(&doc)).unwrap() {
            sources = next;
        }
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("跨巨大容器强调必须按原块真实配对，视口末端不能伪造 EOF");
        };
        for index in window {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                MarkdownPresentationStyles::BLOCK_QUOTE.union(MarkdownPresentationStyles::STRONG)
            );
        }
    }
    let after = request.total_work();
    assert!(after.source_bytes > before.source_bytes);
    assert_eq!(after.copied_source_bytes, 0);
    assert_eq!(after.parser_input_bytes, 0);
    assert_eq!(after.allocated_source_capacity, 0);
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(sources);
    drop(request);
    drop(doc);
    assert_eq!(observer.mapped_bytes(), 0);
    assert_eq!(observer.managed_resident_bytes(), 0);
}

#[test]
fn production_scoped_quote_prefix_visibility_uses_actual_physical_line_endpoints() {
    let source = "> - ``第一行\r\n>   \t第二行😀`` 末尾\r\n>   结束\r\n";
    let second = source.find("\r\n").unwrap() + 2;
    let second = source[..second].encode_utf16().count();
    let length = source.encode_utf16().count();
    for selection in [
        MarkdownTextSelection::caret(0),
        MarkdownTextSelection::caret(second),
        MarkdownTextSelection::caret(length),
        MarkdownTextSelection {
            anchor_utf16: length,
            head_utf16: 0,
        },
    ] {
        let (_directory, mut doc) = document(source);
        doc.set_selection(selection).unwrap();
        let mapping = doc.original_backing_observer().unwrap();
        let (request, sources) = leaf_sources(&doc, 0..length);
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一原请求的真实引用 code 必须发布");
        };
        let mut oracle = MarkdownViewportPresentation::default();
        oracle.set_active_selection(ordered_selection(selection));
        oracle.rebuild(doc.generation(), 0..length, source).unwrap();
        for line in sources.ordinary_lines_for(&doc, authority(&doc)).unwrap() {
            for index in line.prefix_utf16_range() {
                let actual = ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles;
                let expected = oracle.query(index).unwrap().styles;
                for flag in [
                    MarkdownPresentationStyles::MARKER,
                    MarkdownPresentationStyles::HIDDEN_MARKER,
                ] {
                    assert_eq!(
                        actual.contains(flag),
                        expected.contains(flag),
                        "selection={selection:?} index={index}"
                    );
                }
            }
        }
        // Actual code events cross quote/list prefixes; their tabs and extra indentation still retain
        // the original code styling and width authority.
        for (event, range) in Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        {
            if matches!(event, Event::Code(_)) {
                for index in units(source, range) {
                    assert_eq!(
                        ready
                            .query_for(&doc, authority(&doc), index)
                            .unwrap()
                            .unwrap()
                            .styles,
                        oracle.query(index).unwrap().styles,
                        "selection={selection:?} index={index}"
                    );
                }
            }
        }
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(std::fs::read(doc.path()).unwrap(), source.as_bytes());
        drop(sources);
        drop(request);
        drop(doc);
        assert!(mapping.unmap_succeeded() && !mapping.failed());
    }
}
