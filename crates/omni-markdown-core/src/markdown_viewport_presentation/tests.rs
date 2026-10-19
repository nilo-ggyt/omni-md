//! Regression tests preserved with their owning document/workspace boundary.

use super::{
    MarkdownPresentationMode, MarkdownPresentationStyles, MarkdownTableAlignment,
    MarkdownViewportPresentation,
};
use std::rc::Rc;

#[test]
fn borrowed_table_semantics_survive_same_generation_cache_replacement_without_copying() {
    // 第一份实际解析同时产生表、粗体、实体和隐藏标记；快照必须共用这些原始目录。
    let first =
        "| 名称 | 内容 |\n| --- | --- |\n| 甲😀 | **粗体** &amp; [链接](https://example.test) |\n";
    let mut presentation = MarkdownViewportPresentation::default();
    presentation
        .rebuild(7, 0..first.encode_utf16().count(), first)
        .unwrap();
    let original = presentation.semantic_snapshot().unwrap();
    let borrowed = original.clone();
    assert!(Rc::ptr_eq(&original.publication, &borrowed.publication));
    assert_eq!(original.tables().as_ptr(), presentation.tables().as_ptr());
    let marker = first[..first.find("**粗体").unwrap()]
        .encode_utf16()
        .count();
    let before = original.query(marker).unwrap();
    assert!(
        before
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    let retained = Rc::downgrade(&original.publication);
    let table_pointer = original.tables().as_ptr();

    // 正文代次未变，但 TextKit 可转向另一属性窗口；旧单元格不能跟着读新窗口或默认样式。
    let second = "普通窗口，没有表格。";
    presentation
        .rebuild(7, 1000..1000 + second.encode_utf16().count(), second)
        .unwrap();
    assert!(presentation.query(marker).is_none());
    assert!(presentation.tables().is_empty());
    assert_eq!(original.tables().as_ptr(), table_pointer);
    assert_eq!(borrowed.query(marker), Some(before));
    assert!(original.query(original.utf16_window().end).is_none());

    // 当前缓存失效不破坏外借不可变值；最后一个真实借用结束后才释放旧 metadata。
    presentation.invalidate_document_generation();
    assert!(presentation.semantic_snapshot().is_none());
    drop(original);
    assert!(retained.upgrade().is_some());
    drop(borrowed);
    assert!(retained.upgrade().is_none());
}

#[test]
fn fallback_source_selection_and_failed_build_do_not_fabricate_a_semantic_snapshot() {
    let source = "**内容😀**";
    let range = 0..source.encode_utf16().count();
    let mut presentation = MarkdownViewportPresentation::default();
    assert!(presentation.semantic_snapshot().is_none());
    presentation.rebuild(3, range.clone(), source).unwrap();
    let original = presentation.semantic_snapshot().unwrap();
    let publication = Rc::downgrade(&original.publication);

    // 失败重建保持原已验证值，而不是发布参数不匹配的半份目录。
    assert!(presentation.rebuild(3, 0..1, source).is_err());
    assert!(Rc::ptr_eq(
        &original.publication,
        &presentation.semantic_snapshot().unwrap().publication
    ));
    presentation.set_active_selection(1..1);
    assert!(presentation.semantic_snapshot().is_none());
    assert!(original.query(0).is_some());

    // fallback 的可读属性覆盖与完整解析完成分开；空 runs 不能被升级为语义 Ready。
    presentation
        .record_source_fallback_window(3, range.clone())
        .unwrap();
    assert!(presentation.query(0).is_some());
    assert!(presentation.semantic_snapshot().is_none());
    presentation.set_mode(MarkdownPresentationMode::Source);
    presentation.record_source_window(3, range).unwrap();
    assert!(presentation.semantic_snapshot().is_none());
    drop(original);
    assert!(publication.upgrade().is_none());
}

/// Build a current bounded presentation fixture with explicit absolute origin and selection.
fn rebuilt_presentation(
    source: &str,
    absolute_start: usize,
    active_selection: std::ops::Range<usize>,
) -> MarkdownViewportPresentation {
    // Compute the exact UTF-16 window from fixture text and parse it under one known generation.
    let mut presentation = MarkdownViewportPresentation::default();
    presentation.set_active_selection(active_selection);
    let utf16_end = absolute_start + source.encode_utf16().count();
    presentation
        .rebuild(7, absolute_start..utf16_end, source)
        .expect("build bounded Markdown presentation");
    presentation
}

/// Translate fixture byte locations through an independent scalar-boundary oracle.
fn utf16_offset_at_byte(source: &str, byte_offset: usize) -> usize {
    // Assert the fixture location is a scalar boundary before counting its UTF-16 prefix.
    assert!(source.is_char_boundary(byte_offset));
    source[..byte_offset].encode_utf16().count()
}

#[test]
/// Protect all supported semantic style families in one representative Markdown envelope.
fn commonmark_gfm_and_confirmed_extensions_receive_semantic_styles() {
    // Mix confirmed block and inline extensions, then combine the produced style bits.
    let source = [
        "# Heading",
        "",
        "**bold** *emphasis* ~~strike~~ `code` [link](https://example.test)",
        "> quote",
        "- bullet",
        "1. ordered",
        "- [ ] task",
        "---",
        "```rust",
        "fn main() {}",
        "```",
        "| Name | Value |",
        "| --- | ---: |",
        "| Omni | Fast |",
        "$x + y$ ^sup^ <u>underlined</u>",
    ]
    .join("\n");
    let presentation = rebuilt_presentation(&source, 0, source.encode_utf16().count()..0);
    let styles =
        presentation
            .runs()
            .iter()
            .fold(MarkdownPresentationStyles::default(), |combined, run| {
                // Combine independent style roles so the fixture can assert support for every enabled extension.
                combined.union(run.styles)
            });

    // Each supported role must appear independently so a missing parser option cannot silently drop it.
    for expected in [
        MarkdownPresentationStyles::HEADING_1,
        MarkdownPresentationStyles::STRONG,
        MarkdownPresentationStyles::EMPHASIS,
        MarkdownPresentationStyles::STRIKETHROUGH,
        MarkdownPresentationStyles::INLINE_CODE,
        MarkdownPresentationStyles::LINK,
        MarkdownPresentationStyles::BLOCK_QUOTE,
        MarkdownPresentationStyles::UNORDERED_LIST,
        MarkdownPresentationStyles::ORDERED_LIST,
        MarkdownPresentationStyles::TASK_LIST,
        MarkdownPresentationStyles::HORIZONTAL_RULE,
        MarkdownPresentationStyles::FENCED_CODE,
        MarkdownPresentationStyles::TABLE,
        MarkdownPresentationStyles::FORMULA,
        MarkdownPresentationStyles::SUPERSCRIPT,
        MarkdownPresentationStyles::UNDERLINE,
    ] {
        // Check each independent feature bit rather than allowing a nonempty aggregate to hide a missing extension.
        assert!(styles.contains(expected), "missing style {expected:?}");
    }
}

#[test]
/// 单~下标、双~~删除线和上标独立；组合样式不能把中文或emoji重新解释成删除线。
fn subscript_is_distinct_from_strike_and_composes_with_inline_styles() {
    let source = "H~2~O ~~删除~~ ^上标^ ~**中文😀**~ ~*斜体*~ ~[链接](https://example.test)~";
    let length = source.encode_utf16().count();
    let presentation = rebuilt_presentation(source, 0, length + 1..length + 1);
    for (text, expected) in [
        ("2", MarkdownPresentationStyles::SUBSCRIPT),
        ("删除", MarkdownPresentationStyles::STRIKETHROUGH),
        ("上标", MarkdownPresentationStyles::SUPERSCRIPT),
        (
            "中文",
            MarkdownPresentationStyles::SUBSCRIPT.union(MarkdownPresentationStyles::STRONG),
        ),
        (
            "😀",
            MarkdownPresentationStyles::SUBSCRIPT.union(MarkdownPresentationStyles::STRONG),
        ),
        (
            "斜体",
            MarkdownPresentationStyles::SUBSCRIPT.union(MarkdownPresentationStyles::EMPHASIS),
        ),
        (
            "链接",
            MarkdownPresentationStyles::SUBSCRIPT.union(MarkdownPresentationStyles::LINK),
        ),
    ] {
        let start = utf16_offset_at_byte(source, source.find(text).expect("真实内联正文"));
        for index in start..start + text.encode_utf16().count() {
            // 同时查询emoji高低surrogate，语义必须覆盖完整真实UTF16而非UTF8字节下标。
            let styles = presentation.query(index).expect("完整正文语义").styles;
            assert!(styles.contains(expected), "{text}: {styles:?}");
            if expected.contains(MarkdownPresentationStyles::SUBSCRIPT) {
                assert!(!styles.contains(MarkdownPresentationStyles::STRIKETHROUGH));
            } else {
                assert!(!styles.contains(MarkdownPresentationStyles::SUBSCRIPT));
            }
        }
    }
    for (marker, width) in [("~2~", 1), ("~~删除~~", 2)] {
        let start = utf16_offset_at_byte(source, source.find(marker).expect("真实定界符"));
        let query = presentation.query(start).expect("非活动定界符");
        assert_eq!(query.utf16_range, start..start + width);
        assert!(query.styles.contains(MarkdownPresentationStyles::MARKER));
        assert!(
            query
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );
    }
}

#[test]
/// 词内单~仅由Subscript选项授权；关闭时保留旧单~删除线规则，双~~和代码/逃逸事件完全不变。
fn subscript_intraword_is_option_gated_and_preserves_existing_parser_rules() {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let disabled = Options::ENABLE_STRIKETHROUGH;
    let enabled = disabled | Options::ENABLE_SUBSCRIPT;
    for source in ["H~2~O", "水H~😀~O"] {
        let old: Vec<_> = Parser::new_ext(source, disabled)
            .into_offset_iter()
            .collect();
        let new: Vec<_> = Parser::new_ext(source, enabled)
            .into_offset_iter()
            .collect();
        assert!(
            !old.iter().any(|(event, _)| matches!(
                event,
                Event::Start(Tag::Subscript | Tag::Strikethrough)
            ))
        );
        assert!(
            new.iter()
                .any(|(event, _)| matches!(event, Event::Start(Tag::Subscript)))
        );
        assert!(
            !new.iter()
                .any(|(event, _)| matches!(event, Event::Start(Tag::Strikethrough)))
        );
    }
    // 选项关闭下，非词内的单~仍可按旧Parser识别为Strike，而不是被此次修复静默移除。
    assert!(
        Parser::new_ext("~2~", disabled)
            .any(|event| matches!(event, Event::Start(Tag::Strikethrough)))
    );
    for source in ["~~删除~~", r"H\~2\~O", "`H~2~O`", "```text\nH~2~O\n```\n"] {
        let old: Vec<_> = Parser::new_ext(source, disabled)
            .into_offset_iter()
            .collect();
        let new: Vec<_> = Parser::new_ext(source, enabled)
            .into_offset_iter()
            .collect();
        assert_eq!(old, new, "双定界符/转义/代码事件及源坐标应保持：{source}");
    }
    for source in ["H~ 2~O", "H~2 ~O"] {
        // 仅改变词内准入；开头或结尾相邻空白继续由原flanking检查拒绝，不能为通过样本吞空格。
        assert!(
            !Parser::new_ext(source, enabled)
                .any(|event| matches!(event, Event::Start(Tag::Subscript)))
        );
    }
}

#[test]
/// 转义和两种代码区域沿Parser原规则保持字面波浪号，不由自定义扫描补造下标或删除线。
fn escaped_tildes_and_code_do_not_acquire_subscript_or_strike() {
    let source = "\\~普通\\~ `H~2~O ~~原样~~`\n\n```text\nH~2~O ~~原样~~\n```\n";
    let length = source.encode_utf16().count();
    let presentation = rebuilt_presentation(source, 0, length + 1..length + 1);
    assert!(presentation.runs().iter().all(|run| {
        !run.styles.contains(MarkdownPresentationStyles::SUBSCRIPT)
            && !run
                .styles
                .contains(MarkdownPresentationStyles::STRIKETHROUGH)
    }));
    // 代码确实被Parser识别，避免全为空样式把“不误解释”变成无效断言。
    assert!(
        presentation
            .runs()
            .iter()
            .any(|run| { run.styles.contains(MarkdownPresentationStyles::INLINE_CODE) })
    );
    assert!(
        presentation
            .runs()
            .iter()
            .any(|run| { run.styles.contains(MarkdownPresentationStyles::FENCED_CODE) })
    );
}

#[test]
/// 非零文档原点下只显露活动下标的两侧单~，邻接的中文emoji下标继续隐藏自己的语法。
fn subscript_markers_follow_the_original_active_selection_and_utf16_origin() {
    let source = "😀 H~2~O，~中文😀~。";
    let origin = 137;
    let first = origin + utf16_offset_at_byte(source, source.find("~2~").expect("首下标"));
    let second = origin + utf16_offset_at_byte(source, source.find("~中文").expect("后下标"));
    for selection in [
        first + 1..first + 1,
        first + 1..first + 2,
        first + 2..first + 1,
    ] {
        let presentation = rebuilt_presentation(source, origin, selection);
        for marker in [first, first + 2] {
            let query = presentation.query(marker).expect("活动下标单~");
            assert_eq!(query.utf16_range, marker..marker + 1);
            assert!(query.styles.contains(MarkdownPresentationStyles::MARKER));
            assert!(
                !query
                    .styles
                    .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            );
        }
        let content = presentation.query(first + 1).expect("活动下标正文");
        assert!(
            content
                .styles
                .contains(MarkdownPresentationStyles::SUBSCRIPT)
        );
        assert!(
            !content
                .styles
                .contains(MarkdownPresentationStyles::STRIKETHROUGH)
        );
        assert!(
            presentation
                .query(second)
                .expect("后下标语法")
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );
    }
}

#[test]
/// Source不解释单~或双~~，切回Preview才恢复下标；纯合同不把此断言代替Native字号/基线验证。
fn source_keeps_subscript_literal_and_preview_restores_its_semantic_role() {
    let source = "中文😀 H~2~O ~~删除~~";
    let length = source.encode_utf16().count();
    let mut presentation = MarkdownViewportPresentation::default();
    presentation.set_mode(MarkdownPresentationMode::Source);
    presentation
        .rebuild(7, 0..length, source)
        .expect("字面源码窗口");
    for index in 0..length {
        assert!(
            presentation
                .query(index)
                .expect("字面源码覆盖")
                .styles
                .is_empty()
        );
    }
    assert!(presentation.runs().is_empty());
    assert_eq!(
        presentation.set_mode(MarkdownPresentationMode::InlinePreview),
        Some(0..length)
    );
    presentation.set_active_selection(length + 1..length + 1);
    presentation
        .rebuild(7, 0..length, source)
        .expect("恢复语义预览");
    let offset = utf16_offset_at_byte(source, source.find('2').expect("下标正文"));
    let styles = presentation.query(offset).expect("下标语义").styles;
    assert!(styles.contains(MarkdownPresentationStyles::SUBSCRIPT));
    assert!(!styles.contains(MarkdownPresentationStyles::STRIKETHROUGH));
}

#[test]
/// Keep literal source mode free of Markdown semantic runs and parser-owned metadata.
fn source_mode_does_not_create_preview_runs() {
    // Rebuild recognizable Markdown in source mode so accidental parsing would be observable.
    let source = "# **heading**";
    let mut presentation = MarkdownViewportPresentation::default();
    assert_eq!(
        presentation.set_mode(MarkdownPresentationMode::Source),
        None
    );
    presentation
        .rebuild(3, 0..source.encode_utf16().count(), source)
        .expect("record source-mode viewport");

    // Queries must cover the source window with empty styles and retain no semantic runs.
    assert!(presentation.runs().is_empty());
    assert!(presentation.tables().is_empty());
    assert!(!presentation.needs_rebuild(3, 1));
    assert_eq!(
        presentation.cached_utf16_window(),
        Some(0..source.encode_utf16().count())
    );
}

#[test]
/// 源码选区保留有限覆盖，随后切回预览必须使用最后选区恢复正确的语法显隐。
fn source_selection_keeps_coverage_and_preview_uses_latest_selection() {
    let source = "## 标题\n\n**粗体**";
    let length = source.encode_utf16().count();
    let active = source[..source.find("粗体").expect("粗体正文")]
        .encode_utf16()
        .count();
    let mut presentation = MarkdownViewportPresentation::default();
    presentation.set_mode(MarkdownPresentationMode::Source);
    presentation
        .record_source_window(7, 0..length)
        .expect("真实有限源码覆盖");

    // 反向选区仍记录归一化范围，但不能仅因选区变化清空已有字面覆盖。
    assert_eq!(presentation.set_active_selection(active + 1..active), None);
    assert_eq!(presentation.cached_utf16_window(), Some(0..length));
    assert!(!presentation.needs_rebuild(7, active));
    assert!(presentation.runs().is_empty());
    assert!(presentation.tables().is_empty());

    // 模式变化继续返回原覆盖作属性失效；不沿用源码状态伪装成完整预览。
    assert_eq!(
        presentation.set_mode(MarkdownPresentationMode::InlinePreview),
        Some(0..length)
    );
    assert!(presentation.needs_rebuild(7, active));
    presentation
        .rebuild(7, 0..length, source)
        .expect("按最后选区重建预览");

    // 活动粗体标记须显露，离开后的标题标记须隐藏，证明更新没有丢失新选区。
    assert!(
        presentation
            .query(0)
            .expect("标题标记")
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    assert!(
        !presentation
            .query(active - 2)
            .expect("粗体标记")
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    // 回到预览后选区变化仍触发原有失效，不能把优化扩大到依赖选区的模式。
    assert_eq!(presentation.set_active_selection(1..1), Some(0..length));
}

#[test]
/// Keep native popup indices stable and reject unsupported future mode values.
fn popup_mode_mapping_is_bidirectional_and_rejects_future_values() {
    // Check both directions for each supported mode and explicitly reject an unknown index.
    assert_eq!(MarkdownPresentationMode::InlinePreview.popup_index(), 0);
    assert_eq!(MarkdownPresentationMode::Source.popup_index(), 1);
    assert_eq!(
        MarkdownPresentationMode::from_popup_index(0),
        Some(MarkdownPresentationMode::InlinePreview)
    );
    assert_eq!(
        MarkdownPresentationMode::from_popup_index(1),
        Some(MarkdownPresentationMode::Source)
    );
    assert_eq!(MarkdownPresentationMode::from_popup_index(2), None);
    assert_eq!(MarkdownPresentationMode::from_popup_index(usize::MAX), None);
}

#[test]
/// Preserve bounded native invalidation coverage without retaining parsed source-mode metadata.
fn source_window_is_recorded_once_and_invalidated_by_generation_change() {
    // Record one literal window and verify a same-generation position is already covered.
    let mut presentation = MarkdownViewportPresentation::default();
    presentation.set_mode(MarkdownPresentationMode::Source);
    presentation
        .record_source_window(11, 20..80)
        .expect("record bounded source window");
    assert!(!presentation.needs_rebuild(11, 20));
    assert!(!presentation.needs_rebuild(11, 79));
    assert!(presentation.needs_rebuild(11, 80));
    assert_eq!(presentation.cached_utf16_window(), Some(20..80));

    // Generation invalidation must return only the previous window and make cached queries unavailable.
    let invalidated = presentation.invalidate_document_generation();
    assert_eq!(invalidated, Some(20..80));
    assert!(presentation.needs_rebuild(11, 20));
    assert_eq!(presentation.cached_utf16_window(), None);
}

#[test]
/// Reveal source syntax for the selected inline structure without exposing unrelated delimiters.
fn only_the_active_inline_structure_reveals_its_markers() {
    // Place two independently owned inline structures in one viewport and select only the first.
    let source = "**first** and *second*";
    let active = source.find("first").expect("first structure") + 2;
    let presentation = rebuilt_presentation(source, 0, active..active);
    let first_marker = presentation.query(0).expect("first strong marker");
    let second_start = source.find("*second*").expect("second structure");
    let second_marker = presentation
        .query(second_start)
        .expect("second emphasis marker");

    // The first marker remains visible while the second marker keeps its hidden role.
    assert!(
        first_marker
            .styles
            .contains(MarkdownPresentationStyles::MARKER)
    );
    assert!(
        !first_marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    assert!(
        second_marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Project parser byte ranges through Unicode into correctly offset absolute native coordinates.
fn unicode_byte_offsets_become_absolute_utf16_ranges_inside_the_window() {
    // Combine a surrogate-pair prefix, CJK strong text, and a combining suffix at a nonzero viewport origin.
    let source = "😀 **中文** e\u{301}";
    let absolute_start = 41;
    let presentation = rebuilt_presentation(source, absolute_start, 0..0);
    let strong_marker_local_utf16 = "😀 ".encode_utf16().count();
    let marker = presentation
        .query(absolute_start + strong_marker_local_utf16)
        .expect("strong marker after surrogate pair");

    // Marker and text ranges must match independently counted UTF-16 positions with no interior encoding boundaries.
    assert_eq!(
        marker.utf16_range,
        absolute_start + strong_marker_local_utf16..absolute_start + strong_marker_local_utf16 + 2
    );
    assert!(
        marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    assert!(presentation.runs().iter().all(|run| {
        run.utf16_range.start >= absolute_start
            && run.utf16_range.end <= absolute_start + source.encode_utf16().count()
    }));
}

#[test]
/// Keep cache invalidation exact across selection, document generation, and presentation mode transitions.
fn generation_selection_and_mode_changes_invalidate_only_the_bounded_window() {
    // Build a small semantic window at a nonzero document origin.
    let source = "# heading\n\nbody";
    let mut presentation = rebuilt_presentation(source, 100, 110..110);
    assert!(!presentation.needs_rebuild(7, 110));
    assert!(presentation.needs_rebuild(8, 110));

    // A changed selection invalidates only that previously cached window.
    let selection_invalidation = presentation
        .set_active_selection(101..101)
        .expect("selection changes preview context");
    assert_eq!(
        selection_invalidation,
        100..100 + source.encode_utf16().count()
    );
    assert!(presentation.needs_rebuild(7, 110));

    // Rebuild, switch mode, and change generation while checking the same bounded invalidation contract.
    presentation
        .rebuild(7, 100..100 + source.encode_utf16().count(), source)
        .expect("rebuild after selection change");
    let mode_invalidation = presentation
        .set_mode(MarkdownPresentationMode::Source)
        .expect("mode changes preview attributes");
    assert_eq!(mode_invalidation, selection_invalidation);
}

#[test]
/// Keep setext heading style separate from its hidden source underline markers.
fn setext_markers_are_hidden_without_underlining_the_heading_text() {
    // Use both supported setext heading levels under an inactive selection.
    let source = "Primary\n=======\n\nSecondary\n---------\n";
    let presentation = rebuilt_presentation(source, 0, source.len()..source.len());
    let first_marker = source.find("=======").expect("setext marker");
    let title = presentation.query(1).expect("heading title");
    let marker = presentation.query(first_marker).expect("setext marker run");

    // Title text must retain heading roles while delimiter rows remain hidden source syntax.
    assert!(title.styles.contains(MarkdownPresentationStyles::HEADING_1));
    assert!(!title.styles.contains(MarkdownPresentationStyles::UNDERLINE));
    assert!(
        marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Restrict underline styling to the exact supported underline-tag allowlist.
fn headings_and_links_are_not_underlined_but_explicit_u_is() {
    // Place a heading, ordinary link, and exact underline pair in one source envelope.
    let source = "# Heading\n\n[link](https://example.test)\n\n<u>underlined</u>";
    let presentation = rebuilt_presentation(source, 0, source.encode_utf16().count()..0);

    // A heading must receive heading styling without inheriting underline.
    let heading = presentation.query(2).expect("heading text run");
    assert!(
        heading
            .styles
            .contains(MarkdownPresentationStyles::HEADING_1)
    );
    assert!(
        !heading
            .styles
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );

    // A link must receive link styling without automatically inheriting underline.
    let link_start = source.find("[link]").expect("link start");
    let link = presentation.query(link_start + 1).expect("link text run");
    assert!(link.styles.contains(MarkdownPresentationStyles::LINK));
    assert!(!link.styles.contains(MarkdownPresentationStyles::UNDERLINE));

    // The explicit paired underline content must receive underline while its tags remain source markers.
    let underline_start = source.find("underlined").expect("underline text");
    let underline = presentation
        .query(underline_start)
        .expect("explicit underline run");
    assert!(
        underline
            .styles
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );
}

#[test]
/// Change marker hiding from ownership and selection without changing source coordinates.
fn marker_visibility_changes_only_for_the_active_selection() {
    // Build inactive heading and strong structures in one window.
    let source = "# Heading\n\n**bold**";
    let heading_marker = source.find('#').expect("heading marker");
    let active = source.find("bold").expect("bold text") + 1;
    let presentation = rebuilt_presentation(source, 0, active..active);
    let marker = presentation
        .query(heading_marker)
        .expect("heading marker run");
    assert!(marker.styles.contains(MarkdownPresentationStyles::MARKER));
    assert!(
        marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // Activate the heading marker and verify its syntax is revealed while unrelated strong syntax remains hidden.
    let active_heading = rebuilt_presentation(source, 0, heading_marker..heading_marker + 1);
    let visible_marker = active_heading
        .query(heading_marker)
        .expect("visible marker run");
    assert!(
        visible_marker
            .styles
            .contains(MarkdownPresentationStyles::MARKER)
    );
    assert!(
        !visible_marker
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Preserve editable source ownership for all three CommonMark thematic-break marker families.
fn commonmark_horizontal_rules_collapse_until_their_source_is_active() {
    // Each supported thematic-break spelling must hide while inactive and reveal when the caret activates it.
    for source in ["---\n", "***\n", "___\n"] {
        // Test every supported marker family under inactive and active selections with the same ownership assertions.
        let inactive_selection = source.encode_utf16().count() + 1;
        let inactive = rebuilt_presentation(source, 0, inactive_selection..inactive_selection);
        let marker = inactive.query(0).expect("horizontal rule marker");
        assert!(
            marker
                .styles
                .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
        );
        assert!(
            marker
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );

        let active = rebuilt_presentation(source, 0, 1..1);
        let visible = active.query(1).expect("active horizontal rule source");
        assert!(
            visible
                .styles
                .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
        );
        assert!(
            !visible
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );
    }
}

#[test]
/// Preserve table roles, alignments, source extents, and flat cell ownership from authoritative parser events.
fn gfm_table_uses_parser_cells_and_preserves_six_column_semantics() {
    // Use a six-column Unicode table with varied alignment and nested inline content.
    let source = [
        "| 任务编号 (ID) | 模块名称 (Module) | 运行状态 (Status) | 优先级 (Priority) | 响应时间 (Latency) | 详细说明 (Notes) |",
        "| :--- | :--- | :---: | :---: | ---: | :--- |",
        "| `#101` | **用户鉴权中心** `Auth` | 🟢 正常 | `P0` | `12 ms` | 支持 OAuth 2.0 与 JWT 校验 |",
        "| `#102` | **数据同步通道** `Sync` | 🟡 排队中 | `P1` | `145 ms` | 正在等待跨区域节点就绪 |",
        "| `#103` | **推送网关** `Push` | 🔴 异常 | `P0` | `--` | 触发下游接口超时熔断 |",
        "| `#104` | **报表导出** `Export` | ⚪ 已完成 | `P2` | `3,420 ms` | 生成文件大小约 *12.5 MB* |",
    ]
    .join("\n");
    let inactive_selection = source.encode_utf16().count() + 1;
    let presentation = rebuilt_presentation(&source, 0, inactive_selection..inactive_selection);

    // Left-aligned header content must carry header, table, and left-alignment roles.
    let header_offset =
        utf16_offset_at_byte(&source, source.find("任务编号").expect("left header"));
    let header = presentation
        .query(header_offset)
        .expect("left-aligned table header");
    assert!(
        header
            .styles
            .contains(MarkdownPresentationStyles::TABLE_HEADER)
    );
    assert!(
        header
            .styles
            .contains(MarkdownPresentationStyles::TABLE_ALIGN_LEFT)
    );

    // Centered header content must preserve its explicit alignment role.
    let center_offset =
        utf16_offset_at_byte(&source, source.find("运行状态").expect("center header"));
    let center = presentation
        .query(center_offset)
        .expect("center-aligned table header");
    assert!(
        center
            .styles
            .contains(MarkdownPresentationStyles::TABLE_ALIGN_CENTER)
    );

    // Right-aligned header content must preserve its explicit alignment role.
    let right_offset =
        utf16_offset_at_byte(&source, source.find("响应时间").expect("right header"));
    let right = presentation
        .query(right_offset)
        .expect("right-aligned table header");
    assert!(
        right
            .styles
            .contains(MarkdownPresentationStyles::TABLE_ALIGN_RIGHT)
    );

    // Structural table pipes must remain hidden boundary markers rather than cell text.
    let boundary = presentation.query(0).expect("leading table boundary");
    assert!(
        boundary
            .styles
            .contains(MarkdownPresentationStyles::TABLE_BOUNDARY)
    );
    assert!(
        boundary
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // The delimiter row retains table delimiter semantics and hidden source syntax.
    let delimiter_offset =
        utf16_offset_at_byte(&source, source.find(":---").expect("table delimiter"));
    let delimiter = presentation
        .query(delimiter_offset)
        .expect("collapsed table delimiter");
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::TABLE_DELIMITER)
    );
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // Nested strong and other inline table content must retain both table and inline roles.
    let strong_offset = utf16_offset_at_byte(
        &source,
        source.find("用户鉴权中心").expect("strong table content"),
    );
    assert!(
        presentation
            .query(strong_offset)
            .expect("strong table content style")
            .styles
            .contains(MarkdownPresentationStyles::STRONG)
    );
    let code_offset = utf16_offset_at_byte(
        &source,
        source.find("#101").expect("inline code table content"),
    );
    assert!(
        presentation
            .query(code_offset)
            .expect("inline code table content style")
            .styles
            .contains(MarkdownPresentationStyles::INLINE_CODE)
    );
    let emphasis_offset = utf16_offset_at_byte(
        &source,
        source.find("12.5 MB").expect("emphasis table content"),
    );
    assert!(
        presentation
            .query(emphasis_offset)
            .expect("emphasis table content style")
            .styles
            .contains(MarkdownPresentationStyles::EMPHASIS)
    );

    // Inspect the immutable prototype to verify exact row/cell counts, alignment, flat indices, and source-coordinate coverage.
    let tables = presentation.tables();
    assert_eq!(tables.len(), 1);
    let table = &tables[0];
    assert_eq!(table.rows.len(), 5);
    assert_eq!(table.cells.len(), 30);
    assert!(table.rows[0].is_header);
    for (row_index, row) in table.rows.iter().enumerate() {
        // Rows must index six consecutive cells in the single flat vector without overlap or omission.
        assert_eq!(row.cell_index_range, row_index * 6..row_index * 6 + 6);
        assert_eq!(row.is_header, row_index == 0);
    }
    let expected_alignments = [
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Right,
        MarkdownTableAlignment::Left,
    ];
    for (cell_index, cell) in table.cells.iter().enumerate() {
        // Every cell keeps its original column and the independently expected alignment for that column.
        assert_eq!(cell.column_index, cell_index % 6);
        assert_eq!(cell.alignment, expected_alignments[cell_index % 6]);
    }
    let row_sources = [
        source.lines().next().expect("header row"),
        source.lines().nth(2).expect("first body row"),
        source.lines().nth(3).expect("second body row"),
        source.lines().nth(4).expect("third body row"),
        source.lines().nth(5).expect("fourth body row"),
    ];
    for (row, row_source) in table.rows.iter().zip(row_sources) {
        // Each row's absolute UTF-16 extent must match its exact source line.
        let byte_start = source.find(row_source).expect("table row source range");
        let utf16_start = utf16_offset_at_byte(&source, byte_start);
        assert_eq!(
            row.utf16_range,
            utf16_start..utf16_start + row_source.encode_utf16().count()
        );
    }
    let emoji_offset = utf16_offset_at_byte(
        &source,
        source.find('🟢').expect("supplementary table scalar"),
    );
    assert!(table.cells[8].utf16_range.contains(&emoji_offset));
    assert!(table.cells[8].utf16_range.contains(&(emoji_offset + 1)));
    assert_eq!(table.header_utf16_range.start, 0);
    assert_eq!(
        table.delimiter_utf16_range.start,
        source.lines().next().unwrap().encode_utf16().count() + 1
    );
    assert!(table.utf16_range.end <= source.encode_utf16().count());
}

#[test]
/// Prevent escaped and inline-code pipes from becoming fabricated table boundaries.
fn gfm_escaped_pipes_remain_inside_authoritative_parser_cells() {
    // Use escaped pipe text inside both inline code and ordinary cells.
    let source = "| A | B |\n| --- | --- |\n| `a\\|b` | c\\|d |";
    let presentation = rebuilt_presentation(source, 0, source.encode_utf16().count()..0);
    let table = presentation.tables().first().expect("GFM table prototype");

    // Prototype counts must match parser cells and those pipe positions must retain literal cell styles.
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.cells.len(), 4);
    assert_eq!(&source[table.cells[2].utf16_range.clone()], "`a\\|b`");
    assert_eq!(&source[table.cells[3].utf16_range.clone()], "c\\|d");
    for escaped_pipe in [
        source.find("\\|").unwrap() + 1,
        source.rfind("\\|").unwrap() + 1,
    ] {
        // Both escaped pipes are parser-owned cell content and must never become hidden table-boundary decorations.
        let styles = presentation
            .query(escaped_pipe)
            .expect("escaped pipe presentation")
            .styles;
        assert!(!styles.contains(MarkdownPresentationStyles::TABLE_BOUNDARY));
    }
}

#[test]
/// Discard stale table semantics when an oversized block requires bounded literal fallback.
fn source_fallback_records_a_plain_window_without_parsing_a_partial_table() {
    // First build a valid table so stale metadata would be observable.
    let source = "| A | B |\n| --- | --- |\n| 1 | 2 |\n";
    let mut presentation = rebuilt_presentation(source, 100, 0..0);
    assert!(!presentation.tables().is_empty());

    // Record a different source-fallback window without supplying partial table text to the parser.
    presentation
        .record_source_fallback_window(8, 120..140)
        .expect("record source fallback window");

    // No stale tables or style runs may remain, and only the fallback interval is queryable.
    assert!(presentation.tables().is_empty());
    assert!(presentation.runs().is_empty());
    assert!(!presentation.needs_rebuild(8, 125));
    let query = presentation.query(125).expect("plain fallback query");
    assert!(query.styles.is_empty());
    assert_eq!(query.utf16_range, 120..140);
}

#[test]
/// Keep YAML metadata interpretation tied to the actual document beginning.
fn yaml_metadata_is_enabled_only_at_the_real_document_start() {
    // At absolute offset zero, metadata contents remain literal unstyled source rather than thematic breaks.
    let metadata = "---\ntitle: Omni\n---\n\n# Heading\n";
    let presentation = rebuilt_presentation(metadata, 0, metadata.len()..metadata.len());
    let first = presentation.query(0).expect("metadata source run");
    assert!(
        !first
            .styles
            .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
    );

    // The same delimiter spelling at a nonzero origin must follow ordinary Markdown rule semantics.
    let horizontal_rule = "---\n\nbody\n";
    let absolute_start = 100;
    let presentation = rebuilt_presentation(
        horizontal_rule,
        absolute_start,
        absolute_start + horizontal_rule.len()..absolute_start + horizontal_rule.len(),
    );
    let rule = presentation
        .query(absolute_start)
        .expect("non-leading horizontal rule");
    assert!(
        rule.styles
            .contains(MarkdownPresentationStyles::HORIZONTAL_RULE)
    );
}

#[test]
/// Keep native table placeholders independent of selection-sensitive inline marker visibility.
fn inline_table_source_markers_remain_hidden_for_active_selection() {
    // Build a compact table with the selection inside it and inspect its delimiter semantics.
    let source = "Name | Value\n:--- | ---:\nOmni | Fast";
    let delimiter_offset = source.find(":---").expect("table delimiter");
    let presentation = rebuilt_presentation(source, 0, delimiter_offset + 1..delimiter_offset + 1);
    let delimiter = presentation
        .query(delimiter_offset)
        .expect("active delimiter source");
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::TABLE_DELIMITER)
    );
    assert!(
        delimiter
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );

    // Header pipes remain hidden table boundaries even while the native table is being edited.
    let header_boundary = presentation
        .query(source.find('|').expect("header boundary"))
        .expect("header boundary run");
    assert!(
        header_boundary
            .styles
            .contains(MarkdownPresentationStyles::TABLE_BOUNDARY)
    );
    assert!(
        header_boundary
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// Keep table prototypes and semantic runs within the supplied fixed-size UTF-16 window.
fn table_presentation_remains_inside_a_4096_utf16_window() {
    // Embed a Unicode table in source padded to exactly 4096 UTF-16 units.
    let table = "| 名称 | 值 |\n| :--- | ---: |\n| Omni | 高性能 |\n";
    let suffix_length = 4_096 - table.encode_utf16().count();
    let source = format!("{table}{}", "x".repeat(suffix_length));
    let presentation = rebuilt_presentation(&source, 500, 4_597..4_597);

    // Every published semantic interval must remain within the declared absolute window.
    assert_eq!(source.encode_utf16().count(), 4_096);
    assert!(
        presentation
            .runs()
            .iter()
            .all(|run| { run.utf16_range.start >= 500 && run.utf16_range.end <= 4_596 })
    );
}

#[test]
/// 重置必须释放 runs/tables 的容量及旧选择窗口，不能用清空长度冒充资源结束。
fn reset_presentation_releases_every_document_derived_capacity() {
    let mut presentation =
        rebuilt_presentation("# Title\n\n| A | B |\n| --- | --- |\n| x | y |\n", 0, 0..0);
    assert!(!presentation.is_unallocated());
    // 已派生内容失效后仍可能保留分配，只有 take/drop 整个状态才是释放边界。
    presentation.invalidate_document_generation();
    assert!(!presentation.is_unallocated());
    let old = std::mem::take(&mut presentation);
    drop(old);
    assert!(presentation.is_unallocated());
    presentation.set_active_selection(1..1);
    assert!(!presentation.is_unallocated());
}

#[test]
/// quote只显露原选区两个端点所在行；反向选择与正向选择保持同一显隐结果。
fn quote_markers_use_endpoint_lines_and_next_line_start_is_not_previous_line() {
    let source = "> first\n> second\n> third\n> fourth\n";
    let starts: Vec<_> = source
        .match_indices('>')
        .map(|(offset, _)| offset)
        .collect();
    let forward = rebuilt_presentation(source, 0, starts[0] + 3..starts[2] + 3);
    let reverse = rebuilt_presentation(source, 0, starts[2] + 3..starts[0] + 3);
    assert_eq!(forward.runs(), reverse.runs());
    for (line, start) in starts.iter().enumerate() {
        // 跨行选择中间的第二行仍隐藏，第四行也不因同quote结构而显露。
        let styles = forward.query(*start).unwrap().styles;
        assert!(styles.contains(MarkdownPresentationStyles::MARKER));
        assert_eq!(
            styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            !matches!(line, 0 | 2)
        );
    }
    let next_line = rebuilt_presentation(source, 0, starts[1]..starts[1]);
    assert!(
        next_line
            .query(starts[0])
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    assert!(
        !next_line
            .query(starts[1])
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// CRLF、前缀空白、UTF16非零原点和EOF端点不改变物理行归属或正文坐标。
fn quote_markers_preserve_crlf_unicode_origin_and_unterminated_eof() {
    let source = "  > 中文😀\r\n > 次行\r\n> 尾部";
    let origin = 191;
    let length = source.encode_utf16().count();
    let eof = rebuilt_presentation(source, origin, origin + length..origin + length);
    let markers: Vec<_> = source
        .match_indices('>')
        .map(|(offset, _)| origin + utf16_offset_at_byte(source, offset))
        .collect();
    assert!(
        !eof.query(origin)
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::MARKER)
    );
    for (index, marker) in markers.iter().enumerate() {
        let styles = eof.query(*marker).unwrap().styles;
        assert_eq!(
            styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            index != 2
        );
        assert_eq!(eof.query(*marker + 1).unwrap().styles, styles);
    }
    let second = rebuilt_presentation(source, origin, markers[1] - 1..markers[1] - 1);
    assert!(
        second
            .query(markers[0])
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
    assert!(
        !second
            .query(markers[1])
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
    );
}

#[test]
/// 真实嵌套、跨行inline code和quoted code前缀各消费一次；字面`>`不得被吞掉。
fn quote_prefixes_follow_parser_containers_without_hiding_code_or_lazy_text() {
    let source = "> outer\n> > inner\n> outer2\n\n> `a\n> b`\n\n> ```\n> > fenced literal\n> ```\n\n>     > indented literal\n\nlazy > ordinary\n\n```\n> fenced outside\n```\n";
    let presentation = rebuilt_presentation(source, 0, source.len() + 1..source.len() + 1);
    let mut expected = Vec::new();
    let mut line_start = 0;
    for line in source.split_inclusive('\n') {
        // 只列夹具中被真实quote包围的行，外部fence与普通正文明确排除。
        if line.starts_with('>') && !line.contains("fenced outside") {
            expected.push(line_start);
            if line == "> > inner\n" {
                expected.push(line_start + 2);
            }
        }
        line_start += line.len();
    }
    for (offset, _) in source.match_indices('>') {
        let styles = presentation.query(offset).unwrap().styles;
        assert_eq!(
            styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            expected.contains(&offset),
            "quote role at {offset}"
        );
        assert_eq!(
            styles.contains(MarkdownPresentationStyles::MARKER),
            expected.contains(&offset),
            "literal > at {offset}"
        );
    }
}

#[test]
/// Item与quote交错时只越过真实opening/continuation缩进，不从lazy行内追找`>`。
fn quote_prefixes_cross_real_list_containers_and_stop_at_lazy_content() {
    let source = "- > quoted\n  > next\n  lazy > text\n\n> - item\n>   > nested\n>   > next\n";
    let presentation = rebuilt_presentation(source, 0, source.len() + 1..source.len() + 1);
    let literal = source.find("> text").unwrap();
    for (offset, _) in source.match_indices('>') {
        assert_eq!(
            presentation
                .query(offset)
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER),
            offset != literal,
            "prefix at {offset}"
        );
    }
}

#[test]
/// 列表token只来自真实Item和Task事件，保留原bullet种类、checkbox原范围与owner。
fn list_markers_are_exact_parser_tokens_and_leave_ordered_numbers_literal() {
    use super::{MarkdownBulletMarker as Bullet, MarkdownPreviewListMarkerKind as Kind};
    let source = "- dash\n+ plus\n* star\n- [ ] todo\n- [X] done\n1. ordered\n2. [x] ordered task\n\n```\n- [ ] code\n```\n\nordinary - [ ] text\n";
    let presentation = rebuilt_presentation(source, 73, 1_000..1_000);
    let expected = [
        ("- dash", Kind::Bullet(Bullet::Dash), 1),
        ("+ plus", Kind::Bullet(Bullet::Plus), 1),
        ("* star", Kind::Bullet(Bullet::Star), 1),
        (
            "- [ ] todo",
            Kind::TaskBullet {
                source: Bullet::Dash,
                checked: false,
            },
            1,
        ),
        ("[ ] todo", Kind::TaskBox { checked: false }, 3),
        (
            "- [X] done",
            Kind::TaskBullet {
                source: Bullet::Dash,
                checked: true,
            },
            1,
        ),
        ("[X] done", Kind::TaskBox { checked: true }, 3),
        ("[x] ordered task", Kind::TaskBox { checked: true }, 3),
    ];
    assert_eq!(presentation.list_markers().len(), expected.len());
    for (text, kind, length) in expected {
        let start = 73 + source.find(text).unwrap();
        let marker = presentation.list_marker_at(start).unwrap();
        assert_eq!(marker.kind, kind);
        assert_eq!(marker.utf16_range, start..start + length);
        assert!(
            marker.owner_utf16_range.start <= start
                && marker.utf16_range.end <= marker.owner_utf16_range.end
        );
        assert!(!marker.active);
        assert!(
            !presentation
                .query(start)
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );
    }
    for text in ["1. ordered", "2. [x]", "- [ ] code", "- [ ] text"] {
        assert!(
            presentation
                .list_marker_at(73 + source.find(text).unwrap())
                .is_none()
        );
    }
}

#[test]
/// 嵌套端点激活祖先item；跨过但未落入的兄弟item不能因区间重叠激活。
fn list_marker_activity_tracks_item_endpoints_and_boundary_queries_do_not_cross_tokens() {
    let source = "- first\n  - nested😀\n- middle\n- last\n";
    let origin = 211;
    let nested = origin + utf16_offset_at_byte(source, source.find("nested").unwrap());
    let last = origin + utf16_offset_at_byte(source, source.find("last").unwrap());
    let presentation = rebuilt_presentation(source, origin, last..nested);
    let markers = presentation.list_markers();
    assert_eq!(markers.len(), 4);
    assert!(markers[0].active && markers[1].active && !markers[2].active && markers[3].active);
    for marker in markers {
        assert_eq!(
            presentation.list_marker_boundary_range_at(marker.utf16_range.start),
            Some(marker.utf16_range.clone())
        );
        assert_eq!(
            presentation.list_marker_at(marker.utf16_range.start),
            Some(marker)
        );
    }
    let gap = markers[0].utf16_range.end;
    assert_eq!(
        presentation.list_marker_boundary_range_at(gap),
        Some(gap..markers[1].utf16_range.start)
    );
    assert!(
        presentation
            .list_marker_boundary_range_at(origin - 1)
            .is_none()
    );
    assert!(
        presentation
            .list_marker_boundary_range_at(origin + source.encode_utf16().count())
            .is_none()
    );
}

#[test]
/// Source、明确fallback与generation失效必须清空glyph权限，往返重新按同一源构建。
fn quote_and_list_source_roundtrip_and_invalidation_release_marker_authority() {
    let source = "> 引用😀\n- [ ] task\n";
    let length = source.encode_utf16().count();
    let mut presentation = rebuilt_presentation(source, 29, 900..900);
    let expected = presentation.runs().to_vec();
    assert!(!presentation.list_markers().is_empty());
    presentation.set_mode(MarkdownPresentationMode::Source);
    presentation.rebuild(7, 29..29 + length, source).unwrap();
    assert!(presentation.runs().is_empty() && presentation.list_markers().is_empty());
    presentation.set_mode(MarkdownPresentationMode::InlinePreview);
    presentation.rebuild(7, 29..29 + length, source).unwrap();
    assert_eq!(presentation.runs(), expected);
    presentation
        .record_source_fallback_window(7, 29..29 + length)
        .unwrap();
    assert!(
        presentation
            .list_marker_at(29 + utf16_offset_at_byte(source, source.find('-').unwrap()))
            .is_none()
    );
    presentation.rebuild(7, 29..29 + length, source).unwrap();
    presentation.invalidate_document_generation();
    assert!(presentation.list_markers().is_empty());
    assert!(presentation.list_marker_boundary_range_at(29).is_none());
    assert_eq!(source, "> 引用😀\n- [ ] task\n");
}

#[test]
/// 过多真实item必须整批拒绝，调用方再显式记录字面fallback，不发布前缀半成品。
fn quote_and_list_metadata_share_the_bounded_decoration_admission() {
    let source = "- item\n".repeat(super::MAX_MARKDOWN_PRESENTATION_DECORATIONS);
    let mut presentation = MarkdownViewportPresentation::default();
    assert_eq!(
        presentation.rebuild(2, 0..source.len(), &source),
        Err(super::MarkdownViewportPresentationError::DecorationLimitExceeded)
    );
    assert!(presentation.runs().is_empty() && presentation.list_markers().is_empty());
    assert!(presentation.cached_utf16_window().is_none());
}

#[test]
/// quote后的tab虚拟列可使真实Item.raw.start回到祖先，仍须沿原容器游标准确找到单bullet。
fn quoted_tab_list_openings_use_parser_order_and_real_column_state() {
    use super::{MarkdownBulletMarker, MarkdownPreviewListMarkerKind};
    for source in [">\t- item\n", ">\t- [ ] task\n", ">\t>\t- [X] 中文😀\r\n"] {
        let origin = 117;
        let presentation = rebuilt_presentation(source, origin, 999..999);
        let bullet = origin + utf16_offset_at_byte(source, source.find('-').unwrap());
        let expected = if source.contains("[ ]") {
            MarkdownPreviewListMarkerKind::TaskBullet {
                source: MarkdownBulletMarker::Dash,
                checked: false,
            }
        } else if source.contains("[X]") {
            MarkdownPreviewListMarkerKind::TaskBullet {
                source: MarkdownBulletMarker::Dash,
                checked: true,
            }
        } else {
            MarkdownPreviewListMarkerKind::Bullet(MarkdownBulletMarker::Dash)
        };
        let token = presentation.list_marker_at(bullet).unwrap();
        assert_eq!(token.kind, expected);
        assert_eq!(token.utf16_range, bullet..bullet + 1);
        for (offset, _) in source.match_indices('>') {
            let start = origin + utf16_offset_at_byte(source, offset);
            assert!(
                presentation
                    .query(start)
                    .unwrap()
                    .styles
                    .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            );
        }
        assert!(presentation.list_marker_at(origin).is_none());
    }
}

#[test]
/// 已完成列表token占用预算时，underline不得先越界分配再在最终发布前报错。
fn underline_reserves_completed_list_marker_records_before_pushing_pair() {
    let limit = super::MAX_MARKDOWN_PRESENTATION_DECORATIONS;
    let source = "<u>x</u>";
    let mut decorations = vec![
        super::MarkdownByteDecoration {
            byte_range: 0..1,
            styles: MarkdownPresentationStyles::MARKER,
            marker_owner: None,
        };
        limit - 2
    ];
    let before = decorations.len();
    assert_eq!(
        super::add_sanitized_underline_structures(source, &mut decorations, 1),
        Err(super::MarkdownViewportPresentationError::DecorationLimitExceeded)
    );
    assert_eq!(
        decorations.len(),
        before,
        "failed pair must not append even its content run"
    );
    // 空underline只有两个marker；精确剩余容量可以完整容纳，不能把空正文凭空计成第三条。
    super::add_sanitized_underline_structures("<u></u>", &mut decorations, 0).unwrap();
    assert_eq!(decorations.len(), limit);
}
