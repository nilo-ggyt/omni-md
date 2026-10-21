//! Long-paragraph links through the original SourceRequest, with a full parser used only as an
//! independent differential oracle in tests.

use super::*;

#[test]
fn long_root_links_keep_complete_labels_large_targets_and_original_selection_styles() {
    // Paragraph, label, and valid target exceed the original 2048-byte finite path. Original grammar
    // must handle destination escapes/entities and title asterisks, without truncating the label,
    // preserving a raw URL, or sending destination delimiters into emphasis matching.
    let label = format!(
        " label **粗体😀** `a]b` {} 尾 ",
        "中文😀 \\[字面\\] ".repeat(400)
    );
    let raw_target = format!("https://example.com/{}", "p\\(a\\)&amp;😀".repeat(400));
    let source = format!(
        "{}*外层 [{label}]({raw_target} \"标题 *不是强调*\") 后文* tail",
        "普通 abc😀 ".repeat(400),
    );
    let open = source.find('[').unwrap();
    let label_start = open + 1;
    let label_end = label_start + label.len();
    let expected_label = units(&source, label_start..label_end);
    let (expected_owner, expected_destination, expected_title) = Parser::new_ext(
        &source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| match event {
        Event::Start(Tag::Link {
            dest_url, title, ..
        }) => Some((range, dest_url.into_string(), title.into_string())),
        _ => None,
    })
    .expect("完整原Parser必须确认同一合法长Link");
    assert!(expected_destination.len() > 2048);
    assert_eq!(expected_title, "标题 *不是强调*");
    for selection in [
        MarkdownTextSelection::caret(source.encode_utf16().count()),
        MarkdownTextSelection::caret(expected_label.start + 2),
        MarkdownTextSelection {
            anchor_utf16: expected_label.end,
            head_utf16: expected_label.start,
        },
    ] {
        let (directory, mut doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        doc.set_selection(selection).unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, selection)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("真实长label及大于2048的合法目标必须完成，不得仍假作默认或General");
        };
        assert_oracle(leaf, &doc, &source, &requested);
        let link = leaf
            .link_at_for(&doc, authority(&doc), expected_label.start + 2)
            .unwrap()
            .unwrap();
        assert_eq!(link.label_utf16_range, expected_label);
        assert_eq!(link.destination.clone().into_string(), expected_destination);
        // Only the independent parser's normalized target may be copied; long label/body copies
        // cannot be hidden in that total.
        assert_eq!(
            request.total_work().copied_source_bytes,
            expected_destination.len()
        );
        // Click authority covers the complete label; destination/title marker bytes are not
        // clickable label text.
        let destination_start = units(&source, label_end + 2..label_end + 2).start;
        assert!(
            leaf.link_at_for(&doc, authority(&doc), destination_start)
                .unwrap()
                .is_none()
        );
        let opener = units(&source, expected_owner.start..expected_owner.start).start;
        assert_eq!(
            leaf.query_for(&doc, authority(&doc), opener)
                .unwrap()
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            selection.head_utf16 == doc.utf16_len(),
        );
        assert_eq!(doc.selection(), selection);
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn long_link_label_emphasis_cannot_pair_with_outer_delimiters() {
    // Outer emphasis may contain the whole link, but unmatched label delimiters cannot steal outer
    // openers or vice versa. Exceed the finite-block limit so its full parser cannot mask missing
    // streaming-scope wiring.
    for text in [
        "*outside [inside*](https://example.com/a) after*",
        "[**inside](https://example.com/b) outside**",
        "**outside [*inside*](https://example.com/c \"*title*\") after**",
    ] {
        let source = format!("{} {text} tail", "普通😀 ".repeat(1000));
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let backing = doc.original_backing_observer().unwrap();
        let requested = units(&source, source.find(text).unwrap()..source.len());
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("Root真实link强调scope必须完成");
        };
        assert_oracle(leaf, &doc, &source, &requested);
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn long_links_resolve_reference_and_image_priority_with_original_parser() {
    // Keep original reference/image fixtures and resolve their priority through the same Global
    // and external stack. Supported syntax cannot remain General; an image is not a clickable link.
    for text in [
        "[外 [内][T]](https://example.com/outer)\n\n[T]: https://example.com/inner\n",
        "![图片](https://example.com/image)\n",
    ] {
        let source = format!("{} {text}", "普通😀 ".repeat(1000));
        assert!(
            Parser::new_ext(
                &source,
                crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
            )
            .any(|event| matches!(event, Event::Start(Tag::Link { .. } | Tag::Image { .. })))
        );
        let (_directory, doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        let start = source.find(text).unwrap();
        let end = source.find("\n\n[T]:").unwrap_or(source.len());
        let requested = units(&source, start..end);
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("同一Global已经能回答原引用/图片，不能再签General空洞");
        };
        assert_oracle(ready, &doc, &source, &requested);
        if let Some(inner) = source.find("内") {
            let index = units(&source, inner..inner).start;
            assert_eq!(
                ready
                    .link_at_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .destination
                    .clone()
                    .into_string(),
                "https://example.com/inner",
            );
            let outer = units(&source, start + 1..start + 1).start;
            assert!(
                ready
                    .link_at_for(&doc, authority(&doc), outer)
                    .unwrap()
                    .is_none()
            );
        } else {
            let label = units(&source, start + 2..start + 2).start;
            assert!(
                ready
                    .link_at_for(&doc, authority(&doc), label)
                    .unwrap()
                    .is_none()
            );
        }
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn active_large_link_target_keeps_real_cancellation_generation_and_release() {
    let source = format!(
        "{} [{}](https://example.com/{}) tail",
        "*a ".repeat(100),
        "普通😀 ".repeat(1200),
        "x&amp;".repeat(1600),
    );
    for cancelled in [true, false] {
        let (directory, mut doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested, 57, doc.selection())
            .unwrap();
        // Observe public work to establish in-progress target normalization without mutating a
        // decoder, descriptor, or private phase.
        let mut decoding = false;
        for _ in 0..source.len() * 100 + 10000 {
            request.advance(&doc, authority(&doc), 4096).unwrap();
            assert!(
                request
                    .sources_for(&doc, authority(&doc))
                    .unwrap()
                    .is_none()
            );
            if request.total_work().normalizer_input_bytes > 2048 {
                decoding = true;
                break;
            }
        }
        assert!(decoding, "真实目标必须已进入流式归一化且未发布");
        let expected = if cancelled {
            request.cancel();
            MarkdownTableRequestError::Cancelled
        } else {
            doc.replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
                .unwrap();
            MarkdownTableRequestError::SourceSuperseded
        };
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(expected)
        );
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(expected)
        );
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn streamed_direct_link_fixtures_keep_complete_parser_targets_and_cross_viewport_labels() {
    // Keep all seven vendor literals, including the long prefix/label/5000-character target. Public
    // Core requests own real mmap frames, not artificial 1/7/1024-byte frames; the shared kernel's
    // own tests cover exact small-fragment decoding.
    let long = format!(
        "{} [中文{} **粗体** `a]b`](/{}/中文&amp;😀) 后",
        "前文 mixed ".repeat(512),
        "标签".repeat(700),
        "a".repeat(5000),
    );
    for (fixture, source) in [
        "pre [中文 **粗体** `a]b` \\[转义](https://example.invalid/a\\(b\\)?x=&amp; \"题&amp;\") post",
        "outer *[内**粗**与`code`](../x)* trailing",
        "[](a)[x](b)",
        "[a ` b](x)",
        "[<span title='['>中文</span>](<目标>)",
        "[x](<a>\r\n\"多行\r\n标题\")",
        &long,
    ]
    .into_iter()
    .enumerate()
    {
        let expected: Vec<_> = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .into_offset_iter()
        .filter_map(|(event, owner)| match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                // These fixed targets/titles contain no literal "](". The independent parser gives
                // the actual owner; that delimiter locates only the fixture label, without deriving
                // endpoints from the tested proof or reimplementing bracket grammar.
                let raw = &source[owner.clone()];
                let close = raw.rfind("](").expect("原固定直接链接的label分隔符");
                assert_eq!(raw.as_bytes()[0], b'[');
                let label = units(source, owner.start + 1..owner.start + close);
                Some((units(source, owner), label, dest_url.into_string()))
            }
            _ => None,
        })
        .collect();
        assert!(!expected.is_empty());
        // Production open maps this isolated file and all continuations borrow the same document.
        // The complete string/parser belong only to independent expectations, never injected as
        // storage, callbacks, or preparsed results for the request under test.
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let selection = MarkdownTextSelection::caret(doc.utf16_len());
        doc.set_selection(selection).unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, selection)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let mut sources = request.take_sources_for(&doc, authority(&doc)).unwrap().unwrap();
        {
            // Require actual RootLeafReady and query through public Source/selection/publication
            // checks; SourcesOnly or an internal span alone does not establish visible semantics.
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) =
                sources.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
            else {
                panic!("原直接链接fixture必须经真实文件端口完成，fixture={fixture}");
            };
            // Positionwise comparison also prevents code/emphasis from crossing original label scopes.
            assert_oracle(ready, &doc, source, &requested);
            for (owner, label, destination) in &expected {
                for index in label.clone() {
                    let actual = ready.link_at_for(&doc, authority(&doc), index).unwrap().unwrap();
                    assert_eq!(&actual.label_utf16_range, label);
                    assert_eq!(actual.destination.clone().into_string(), *destination);
                }
                // Empty labels retain marker styles but no clickable characters; targets/titles
                // acquire no label authority either.
                for index in owner.start..label.start {
                    assert!(ready.link_at_for(&doc, authority(&doc), index).unwrap().is_none());
                }
                for index in label.end..owner.end {
                    assert!(ready.link_at_for(&doc, authority(&doc), index).unwrap().is_none());
                }
            }
        }
        if fixture == 6 {
            // The long fixture cannot use finite-block parsing. Exact normalized URL copy totals
            // reject copying its label, plain prefix, or title into target inventory while claiming
            // zero-copy body processing.
            let expected_bytes: usize = expected.iter().map(|(_, _, target)| target.len()).sum();
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().copied_source_bytes, expected_bytes);
            let (_, label, destination) = &expected[0];
            // Reposition the same original-Global request at the label's start/middle/end. These
            // are actual canonical viewports, not invented SourceFrame sizes; normal advance must
            // publish each new output.
            let middle = label.start + label.len() / 2;
            for viewport in [label.start..label.start + 2, middle..middle + 2, label.end - 3..label.end] {
                request.reposition(&doc, &sources, viewport.clone(), authority(&doc)).unwrap();
                drop(sources);
                // Return old publication before bounded repositioning. New style coverage stays
                // finite, while complete-label click authority comes from the same closed owner,
                // never an old ticket retagged for the new window.
                super::inline::finish_inline(&mut request, &doc).unwrap();
                sources = request.take_sources_for(&doc, authority(&doc)).unwrap().unwrap();
                let MarkdownOrdinarySemanticState::RootLeafReady(ready) =
                    sources.ordinary_semantics_for(&doc, authority(&doc)).unwrap()
                else {
                    panic!("长label的真实有限窗口必须完成");
                };
                assert_oracle(ready, &doc, source, &viewport);
                let actual = ready.link_at_for(&doc, authority(&doc), viewport.start).unwrap().unwrap();
                assert_eq!(&actual.label_utf16_range, label);
                assert_eq!(actual.destination.clone().into_string(), *destination);
            }
        }
        assert_eq!(doc.generation(), 0);
        assert_eq!(doc.selection(), selection);
        assert!(!doc.can_undo() && !doc.can_redo() && !doc.is_dirty());
        assert!(doc.original_file_identity_is_unchanged());
        // Preserve original file identity, selection, history, and bytes on this read-only path.
        // Reopening equal content is not resource release; the observer retains facts without
        // keeping document, Source, or mmap alive.
        assert_eq!(std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(), source.as_bytes());
        drop((sources, request, doc));
        assert!(backing.unmap_succeeded());
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn original_unknown_link_fixtures_now_use_real_resolved_missing_and_image_permissions() {
    // Original literals now require actual outcomes instead of outdated General expectations:
    // missing references/bad targets stay literal, images gain no click target, and a successful
    // inner link disables its outer link according to the independent parser.
    for source in [
        "[reference]",
        "![image](x)",
        "[outer [inner](x)](y)",
        "[x](<bad",
    ] {
        let (directory, doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let selection = doc.selection();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, selection)
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原ReferenceMissing/Image/禁用祖先必须得到真实结果，source={source:?}");
        };
        assert_oracle(ready, &doc, source, &requested);
        let inner = source
            .find("inner")
            .map(|start| units(source, start..start + "inner".len()));
        // The parser accepts only inner x in the nested case; outer brackets stay literal and image
        // alt has style but no click URI. Verify every position, not just existence of some link.
        for index in requested {
            let actual = ready.link_at_for(&doc, authority(&doc), index).unwrap();
            if let Some(label) = inner.as_ref().filter(|label| label.contains(&index)) {
                let actual = actual.unwrap();
                assert_eq!(&actual.label_utf16_range, label);
                assert_eq!(actual.destination.clone().into_string(), "x");
            } else {
                assert!(actual.is_none());
            }
        }
        // After normal publication, cancelling the parent must prevent later advance from
        // reacquiring action authority merely because Source remains healthy.
        request.cancel();
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::Cancelled)
        );
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::Cancelled)
        );
        assert_eq!(doc.generation(), 0);
        assert_eq!(doc.selection(), selection);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        // Drop actual publications/request after cancellation, without a test clear or synthetic zero-resource receipt.
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn long_footnote_scope_remains_explicitly_pending_without_plain_or_link_authority() {
    // Reference/image support does not authorize unsupported long footnotes. Both actual footnote
    // forms exceed the 2048-byte finite-parser limit; a short fixture or default plain suffix must
    // not disguise the missing scope support.
    for source in [
        format!("{} [^f] tail\n\n[^f]: 脚注正文\n", "普通😀 ".repeat(1000)),
        format!("[^f]: {} **粗体** [x](y)\n", "普通😀 ".repeat(1000)),
    ] {
        assert!(
            Parser::new_ext(
                &source,
                crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
            )
            .any(|event| matches!(
                event,
                Event::FootnoteReference(_) | Event::Start(Tag::FootnoteDefinition(_))
            ))
        );
        let (directory, doc) = document(&source);
        let backing = doc.original_backing_observer().unwrap();
        let end = source.find("\n\n").unwrap_or(source.len());
        let requested = units(&source, 0..end);
        // Request only the fixture's first actual paragraph/definition. Global may read later
        // definitions, but that does not grant link/plain authority to unsupported long footnotes.
        let mut request = doc
            .viewport_leaf_source_request(requested, 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        assert!(matches!(
            sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap(),
            MarkdownOrdinarySemanticState::LeafPending(_)
        ));
        // Pending is the actual supported outcome; do not query a fabricated default style for a partial pass.
        assert_eq!(doc.generation(), 0);
        assert!(!doc.can_undo() && !doc.can_redo());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}

#[test]
fn original_url_literals_keep_parser_values_and_source_supersession_after_real_undo() {
    // Keep six original URL literals/independent expected values. Core must return complete valid
    // UTF-8 targets; exact 1/8-byte carry belongs to UnescapeStream's tests, not fake Source frames here.
    for (source, expected) in [
        (r"[x](/a\(b\))", "/a(b)"),
        ("[x](</a&amp;&#x1f600;>)", "/a&😀"),
        ("[x](</a&unknown;z>)", "/a&unknown;z"),
        (r"[x](</a\q>)", r"/a\q"),
        ("[x](</a&#0;>)", "/a�"),
        ("[x]()", ""),
    ] {
        let original: Vec<_> = Parser::new_ext(
            source,
            crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
        )
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.into_string()),
            _ => None,
        })
        .collect();
        assert_eq!(original, [expected]);
        let (directory, mut doc) = document(source);
        let backing = doc.original_backing_observer().unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        super::inline::finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("原合法URL不得因流式化而丢失");
        };
        assert_oracle(ready, &doc, source, &requested);
        let actual = ready
            .link_at_for(&doc, authority(&doc), 1)
            .unwrap()
            .unwrap();
        assert_eq!(actual.label_utf16_range, 1..2);
        assert_eq!(actual.destination.clone().into_string(), expected);
        // Actual finite-block parsing copies this complete short fixture once into body inventory.
        // Vendor decoder URL-output accounting is not this entry's body cost; even an empty URL
        // has parser input. Keep finish_inline's original 2048-byte bound without snapshotting parse
        // counts; complete destination expectations come from the independent literals/parser above.
        assert_eq!(request.total_work().copied_source_bytes, source.len());
        assert!(request.total_work().parser_input_bytes >= source.len());
        // Editing revokes Source; undo restores bytes, not generation or a failed request. Both
        // rejection checks use current healthy authority rather than a permanently cancelled input
        // that could mask lost first-error retention.
        doc.replace_utf16_range(0..0, "新", MarkdownTextSelection::caret(1))
            .unwrap();
        assert_eq!(
            ready.link_at_for(&doc, authority(&doc), 1).err(),
            Some(MarkdownTableRequestError::SourceSuperseded)
        );
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::SourceSuperseded)
        );
        doc.undo().unwrap();
        // Undo cannot revive old-generation authority or reconstruct a decoder to append partial
        // old output; SourceSuperseded must remain the first failure under current healthy input.
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(MarkdownTableRequestError::SourceSuperseded)
        );
        let mut restored = Vec::new();
        doc.write_utf16_range_to(0..doc.utf16_len(), &mut restored)
            .unwrap();
        assert_eq!(restored, source.as_bytes());
        assert_eq!(
            std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
            source.as_bytes()
        );
        drop((sources, request, doc));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }
}
