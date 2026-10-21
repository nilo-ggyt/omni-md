//! Differential checks between the production factory and an independent full parser used only in
//! tests, without another product parser.

use super::production::{finish_sources, leaf_sources};
use super::*;

/// Obtain request coordinates from the independent parser, not the Leaf bounds under test.
fn heading_range(source: &str) -> Range<usize> {
    let bytes = Parser::new_ext(
        source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| matches!(event, Event::Start(Tag::Heading { .. })).then_some(range))
    .unwrap();
    units(source, bytes)
}

#[test]
fn production_atx_plain_levels_markers_unicode_and_line_endings_match_original_parser() {
    for level in 1..=6 {
        let hashes = "#".repeat(level);
        for suffix in ["\n", "\r\n", "\r", ""] {
            for text in [
                "中文😀 e\u{301}",
                "中文😀 ##  \t",
                "中文\t###",
                "中文 ###\t",
                "\t中文 # 尾",
                "",
                "   ",
            ] {
                let source = format!("前言\n\n  {hashes} {text}{suffix}");
                let requested = heading_range(&source);
                for selection in [
                    MarkdownTextSelection::caret(requested.start),
                    MarkdownTextSelection::caret(0),
                    MarkdownTextSelection {
                        anchor_utf16: requested.end,
                        head_utf16: requested.start,
                    },
                ] {
                    let (_directory, mut doc) = document(&source);
                    doc.set_selection(selection).unwrap();
                    // Match Native source_leaf_query: consume the public factory's actual Ready
                    // publication and its authorized query.
                    let (request, sources) = leaf_sources(&doc, requested.clone());
                    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                        .ordinary_semantics_for(&doc, authority(&doc))
                        .unwrap()
                    else {
                        panic!("无复杂inline的真实Root ATX必须可由既有Native查询使用");
                    };
                    assert_oracle(ready, &doc, &source, &requested);
                    assert_eq!(request.total_work().copied_source_bytes, 0);
                    assert_eq!(request.total_work().parser_input_bytes, 0);
                    assert_eq!(request.total_work().allocated_source_capacity, 0);
                }
            }
        }
    }
}

#[test]
fn production_atx_tab_separator_empty_hashes_and_partial_coverage_keep_original_styles() {
    for source in [
        "#\t中文😀\n\n尾\n",
        "#\n\n尾\n",
        "######\t\n\n尾\n",
        "### ###\n\n尾\n",
    ] {
        let requested = heading_range(source);
        let (_directory, mut doc) = document(source);
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let (_request, sources) = leaf_sources(&doc, requested.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
            panic!("Tab分隔或空ATX仍应沿真实块事实完成");
        };
        assert_oracle(ready, &doc, source, &requested);
    }

    let source = format!("## {} ##  \n\n尾\n", "中".repeat(100_000));
    let (_directory, doc) = document(&source);
    let requested = 50_000..50_017;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("巨型标题只发布有限可见交集，不复制整行");
    };
    assert_oracle(ready, &doc, &source, &requested);
    assert_eq!(ready.run_count(), 1);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);

    for tail in [" \t".repeat(50_000), "#".repeat(100_000)] {
        let source = format!("## 中文😀\t##{tail}\n\n尾\n");
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let requested = 50_000..50_017;
        let (request, sources) = leaf_sources(&doc, requested.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
            panic!("超长尾空白或#串也须沿有限反向frame完成真实交集");
        };
        assert_oracle(ready, &doc, &source, &requested);
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
    }
}

#[test]
fn production_atx_fixed_tail_tab_spans_preserve_known_marker_visibility() {
    for (source, owner, heading, opening, closing) in [
        (
            "前言\n\n  # 中文😀 ##  \t\n",
            6..19,
            MarkdownPresentationStyles::HEADING_1,
            6..8,
            Some(13..18),
        ),
        (
            "## 文本\t###\n\n尾\n",
            0..10,
            MarkdownPresentationStyles::HEADING_2,
            0..3,
            Some(6..9),
        ),
        (
            "# 中文 ##\r\n\n尾\n",
            0..9,
            MarkdownPresentationStyles::HEADING_1,
            0..2,
            None,
        ),
        (
            "# 中文 ##\t\n\n尾\n",
            0..9,
            MarkdownPresentationStyles::HEADING_1,
            0..2,
            Some(5..8),
        ),
    ] {
        let (_directory, mut doc) = document(source);
        // A fixed inactive selection gives independent hidden-marker expectations; comparing two
        // consumers of the same shared tail alone would not establish correctness.
        let inactive = if owner.start > 0 { 0 } else { doc.utf16_len() };
        doc.set_selection(MarkdownTextSelection::caret(inactive))
            .unwrap();
        let (_request, sources) = leaf_sources(&doc, owner.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
            panic!("已知纯文本反例必须完成真实ATX交集");
        };
        let mut baseline = MarkdownViewportPresentation::default();
        baseline.set_active_selection(ordered_selection(doc.selection()));
        baseline
            .rebuild(doc.generation(), 0..doc.utf16_len(), source)
            .unwrap();
        for index in owner {
            let marker = opening.contains(&index)
                || closing.as_ref().is_some_and(|range| range.contains(&index));
            let expected = if marker {
                heading
                    .union(MarkdownPresentationStyles::MARKER)
                    .union(MarkdownPresentationStyles::HIDDEN_MARKER)
            } else {
                heading // A tab before the closer stays content; the CRLF fixture's trailing hashes remain visible.
            };
            assert_eq!(baseline.query(index).unwrap().styles, expected);
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                expected
            );
        }
    }
}

#[test]
fn production_atx_large_general_inline_stays_unpublished() {
    for text in [
        "**强调**",
        "*斜体*",
        "[链接](https://example.com)",
        "`代码`",
        "&amp; 实体",
        "\\*转义",
        "<u>下划线</u>",
        "~~删除线~~",
        "~下标~",
        "普通中文之后才有 **强调**",
    ] {
        let source = format!("## {}\n\n尾\n", text.repeat(2049));
        let (_directory, doc) = document(&source);
        let requested = heading_range(&source);
        for range in [
            requested.clone(),
            requested.start..(requested.start + 3).min(requested.end),
        ] {
            let (_request, sources) = leaf_sources(&doc, range);
            assert!(matches!(
                sources.semantic,
                MarkdownOrdinarySemanticState::LeafPending(
                    MarkdownRootLeafPendingReason::NeedsGeneralInline
                )
            )); // A locally plain prefix cannot rule out later heading candidates that affect earlier pairing/styles.
        }
    }
    let source = format!(
        "## **{}**\n\n```\n独立代码\n```\n",
        "标题仍未知".repeat(500)
    );
    let (_directory, doc) = document(&source);
    let (_request, sources) = leaf_sources(&doc, 0..doc.utf16_len());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("未知标题不能取消同窗独立代码的真实覆盖");
    };
    assert!(ready.query_for(&doc, authority(&doc), 0).unwrap().is_none());
    let code = units(&source, first_leaf_range(&source));
    assert!(
        ready
            .query_for(&doc, authority(&doc), code.start)
            .unwrap()
            .is_some()
    );
}

#[test]
fn production_atx_long_literal_dollars_and_carets_keep_heading_layout() {
    // Disabled extensions cannot make literal dollars/carets wait for GeneralInline. The actual
    // bounded factory must style long literal headings against the same-option full-parser oracle.
    for text in ["$普通美元$", "^普通字符^ ²₃"] {
        let source = format!("## {}\n\n尾\n", text.repeat(2049));
        let (_directory, doc) = document(&source);
        let requested = heading_range(&source);
        let (request, sources) = leaf_sources(&doc, requested.clone());
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("美元和插入符字面标题必须沿原RootLeaf排版完成");
        };
        assert_oracle(ready, &doc, &source, &requested);
        // Continue streaming the original Source; supporting long literal headings permits no
        // complete-body copy into a parser.
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
        assert_eq!(request.total_work().allocated_source_capacity, 0);
    }
}

#[test]
fn production_atx_joint_quota_cancel_edit_and_mapping_error_release_original_owners() {
    let source = "## 中文😀 ##\n\n尾\n";
    let (_directory, mut doc) = document(source);
    let requested = heading_range(source);
    let (mut request, previous) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &previous.semantic else {
        panic!("原ATX先产生真实样式以核验后续撤权");
    };
    let root = std::sync::Arc::downgrade(doc.root.as_ref().unwrap());
    let original = doc.original_backing_observer().unwrap();
    doc.insert_utf16(0, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo().unwrap();
    assert_eq!(
        ready.query_for(&doc, authority(&doc), requested.start),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ); // Undo may restore the root/bytes but advances generation, so equal coordinates cannot revive publication.
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    drop(doc);
    assert!(root.upgrade().is_none());
    assert!(original.unmap_succeeded());
    assert_eq!(original.mapped_bytes(), 0);
    assert_eq!(original.managed_resident_bytes(), 0);

    for fault in [false, true] {
        // Exceed the original 64-KiB window so body scanning really pages/reclaims through msync;
        // a cache hit alone would not consume the armed fault.
        let source = format!("## {} ##\n\n尾\n", "中".repeat(100_000));
        let (_directory, doc) = document(&source);
        let observer = doc.original_backing_observer().unwrap();
        let requested = heading_range(&source);
        let (mut request, sources) = source_request(&doc, requested.start..requested.start + 17);
        let mut job = prepare_job(&mut request, &sources, &doc);
        while !matches!(job.phase, Phase::Heading) {
            let work = job
                .advance(
                    &doc,
                    authority(&doc),
                    4096,
                    request.remaining_source_limits().unwrap(),
                    &mut request.semantic,
                    Some(request.batch.source_global_mut().unwrap()),
                )
                .unwrap();
            assert_step(work);
            request.batch.account_source_work(work).unwrap();
        }
        if fault {
            let attempts = observer.reclaim_attempts();
            let successes = observer.reclaim_successes();
            observer.fail_next_reclaim_for_test();
            assert_eq!(
                finish_job(&mut request, &mut job, &doc).err(),
                Some(MarkdownTableRequestError::InvalidRequest)
            ); // Exercise EIO during an actual source-frame return rather than directly failing the semantic job.
            assert!(observer.reclaim_attempts() > attempts);
            assert!(observer.reclaim_successes() > successes);
            assert_eq!(observer.first_errno(), Some(libc::EIO));
            assert_eq!(
                job.advance(
                    &doc,
                    authority(&doc),
                    4096,
                    request.remaining_source_limits().unwrap(),
                    &mut request.semantic,
                    None,
                )
                .err(),
                Some(MarkdownTableRequestError::InvalidRequest)
            ); // The sticky source failure ended this job; later arguments cannot rename it cancellation or revive Ready.
        } else {
            let mut cancelled = authority(&doc);
            cancelled.cancelled = true;
            assert_eq!(
                job.advance(
                    &doc,
                    cancelled,
                    4096,
                    request.remaining_source_limits().unwrap(),
                    &mut request.semantic,
                    None,
                )
                .err(),
                Some(MarkdownTableRequestError::Cancelled)
            );
            assert_eq!(
                job.advance(
                    &doc,
                    authority(&doc),
                    4096,
                    request.remaining_source_limits().unwrap(),
                    &mut request.semantic,
                    None,
                )
                .err(),
                Some(MarkdownTableRequestError::Cancelled)
            ); // Healthy arguments after cancellation cannot resume ATX or contaminate healthy backing I/O facts.
            assert_eq!(observer.first_errno(), None);
        }
        assert!(job.heading_next.is_none());
        assert!(job.record.is_none());
        assert_eq!(job.runs.len, 0);
        drop(doc);
        assert!(observer.unmap_succeeded());
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
        if fault {
            assert_eq!(observer.first_errno(), Some(libc::EIO)); // Actual successful munmap does not erase the original source failure.
        }
    }

    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    quota.admit_retained(quota.remaining_runs(), 0).unwrap();
    assert_eq!(
        finish_sources(&mut request, &doc).err(),
        Some(MarkdownTableRequestError::Window(
            MarkdownTableWindowError::SemanticLimit
        ))
    ); // Exhausted joint inventory grants no fresh ATX budget or empty-run Ready.
    assert!(request.leaf_semantic.is_none());
    assert!(request.records.is_empty());
}
