//! 真实长段落 Link 只通过原 SourceRequest 验收；完整 Parser 仅在测试中作独立差分。

use super::*;

#[test]
fn long_root_links_keep_complete_labels_large_targets_and_original_selection_styles() {
    // 段落、label 与合法目标都越过原2048小块门；目的中的括号/实体与title中的星号
    // 必须由同一原语法处理，不可截断 label、直接保 raw URL 或把目标里的 delimiter 送进强调栈。
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
        // 独立完整Parser的规范化目标是唯一可复制payload，不能把长label/段落复制混入账。
        assert_eq!(
            request.total_work().copied_source_bytes,
            expected_destination.len()
        );
        // 点击范围保完整真实 label；目标/title 的原字节虽然属于 LINK marker，却不变成可点击label。
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
    // 外层的闭合强调可包住整个Link，label内未配对的*却不能夺走外层opener；反向情形也相同。
    // 两条真实段落都超小块上限，不能让完整小块Parser兜住流式scope接线缺失。
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
fn long_links_keep_unresolved_reference_and_image_priority_pending() {
    // 完整原 Parser 确实识别内部引用/图片；首组流式Link尚未取得它们的Global权威时，
    // 不能猜内层'['只是字面、错误签外层目标，也不能把图片冒成普通可点击Link。
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
        let requested = units(&source, start..source.len());
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
        // 只观察公开真实work，确认大目标正在归一化且未发布；不突变decoder/FD/私有phase。
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
