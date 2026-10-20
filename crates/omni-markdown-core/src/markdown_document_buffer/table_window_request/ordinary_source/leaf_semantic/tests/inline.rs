//! 从真实 Source 工厂验收已接入 inline；小块原 Parser 和 giant code 使用不同真实资源口径。

use super::*;

#[test]
/// 视口前另一真实小块的引用也必须先解析；当前普通/富文本目标与已解析链接库存均不能被丢掉。
fn offscreen_finite_reference_before_code_keeps_target_semantics_and_original_global() {
    for prefix in ["", "## "] {
        for target in ["target 普通正文\n", "target **粗体** [当前][T]\n"] {
            for definitions in [
                "[U]: https://example.com/first\n[U]: https://example.com/later\n",
                "",
            ] {
                let source = format!(
                    "<u>open\n\n{prefix}[前置][U] `</u>` offscreen after\n\n{target}\ntrue close</u>\n\n{definitions}[T]: https://example.com/current\n"
                );
                let (_directory, mut doc) = document(&source);
                doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
                    .unwrap();
                let start = source.find("target").unwrap();
                let requested = units(&source, start..start + target.len());
                let mut request = doc
                    .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
                    .unwrap();
                finish_inline(&mut request, &doc).unwrap();
                assert!(
                    !request
                        .batch
                        .source_global_mut()
                        .unwrap()
                        .semantic_boundary_retained(),
                    "完成前置查询重放之后必须归还同一个Global的主恢复票据"
                );
                let sources = request
                    .take_sources_for(&doc, authority(&doc))
                    .unwrap()
                    .unwrap();
                let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
                    panic!("前置有限块的引用不得令当前已闭合目标停留Pending")
                };
                assert_oracle(ready, &doc, &source, &requested);
                assert!(
                    ready
                        .query_for(&doc, authority(&doc), requested.start)
                        .unwrap()
                        .unwrap()
                        .styles
                        .contains(MarkdownPresentationStyles::UNDERLINE)
                );
                if let Some(label) = source.find("当前") {
                    let index = units(&source, label..label).start;
                    assert_eq!(
                        ready
                            .link_at_for(&doc, authority(&doc), index)
                            .unwrap()
                            .unwrap()
                            .destination
                            .clone()
                            .into_string(),
                        "https://example.com/current",
                        "补收前置块查询不能替换当前目标已解析的链接值"
                    );
                }
            }
        }
    }
}

#[test]
/// 两个不同前置块分次增加查询时仍复用原Global与Map，第二次重放不能覆盖当前目标已解析的T。
fn multiple_offscreen_reference_blocks_preserve_target_links_and_finish_all_replays() {
    let source = "<u>open\n\n[第一处][U] `</u>` first offscreen\n\n## [第二处][V] `</u>` second offscreen\n\ntarget **粗体** [当前][T]\n\ntrue close</u>\n\n[U]: https://example.com/u-first\n[U]: https://example.com/u-later\n[V]: https://example.com/v-first\n[V]: https://example.com/v-later\n[T]: https://example.com/current\n";
    let (_directory, mut doc) = document(source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let start = source.find("target").unwrap();
    let requested = units(source, start..start + "target **粗体** [当前][T]\n".len());
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    assert!(
        !request
            .batch
            .source_global_mut()
            .unwrap()
            .semantic_boundary_retained()
    );
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("多个真实前置查询必须结束全部重放并交付当前目标语义")
    };
    assert_oracle(ready, &doc, source, &requested);
    let label = source.find("当前").unwrap();
    assert_eq!(
        ready
            .link_at_for(&doc, authority(&doc), units(source, label..label).start)
            .unwrap()
            .unwrap()
            .destination
            .clone()
            .into_string(),
        "https://example.com/current"
    );
}

#[test]
/// 新增前置查询后的取消和真实映射回收失败都必须释放原恢复义务，且后续操作不能覆盖首因。
fn offscreen_reference_preparation_cancel_and_real_source_error_release_original_owners() {
    let prefix = "[前置][U] `</u>` offscreen after\n";
    // 真定义放到超过原64KiB驻留窗口的后方；新增查询必须实际换页，回收故障才有真实触发点。
    let distant = "中".repeat(40_000);
    let source = format!(
        "<u>open\n\n{prefix}\ntarget **粗体** [当前][T]\n\ntrue close</u>\n\n{distant}\n\n[U]: https://example.com/first\n[T]: https://example.com/current\n"
    );
    for source_failure in [false, true] {
        let (_directory, doc) = document(&source);
        let observer = doc.original_backing_observer().unwrap();
        let start = source.find("target").unwrap();
        let requested = units(&source, start..start + "target **粗体** [当前][T]\n".len());
        let mut request = doc
            .viewport_leaf_source_request(requested, 57, doc.selection())
            .unwrap();
        let mut prepared = false;
        for _ in 0..source.len() * 100 + 10000 {
            let (MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work }) =
                request.advance(&doc, authority(&doc), 4096).unwrap();
            assert!(work.metadata_bytes <= 4096 && work.parser_input_bytes <= 2048);
            if work.parser_input_bytes == prefix.len() {
                prepared = true; // 实际前置块的原Parser已收查询，尚未把未知引用当Missing解释HTML。
                break;
            }
        }
        assert!(prepared);
        assert!(
            request
                .batch
                .source_global_mut()
                .unwrap()
                .semantic_boundary_retained()
        );
        let error = if source_failure {
            let attempts = observer.reclaim_attempts();
            observer.fail_next_reclaim_for_test();
            let error = loop {
                match request.advance(&doc, authority(&doc), 4096) {
                    Ok(MarkdownViewportSourceStep::Progress { .. }) => {}
                    Ok(MarkdownViewportSourceStep::SourcesAvailable { .. }) => {
                        panic!("原映射尚未归还成功，不能发布目标语义")
                    }
                    Err(error) => break error,
                }
            };
            assert_eq!(
                error,
                MarkdownTableRequestError::Source(
                    crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
                )
            );
            assert!(observer.reclaim_attempts() > attempts);
            assert_eq!(observer.first_errno(), Some(libc::EIO));
            request.cancel(); // 真实I/O首因已经固定，迟到的取消不得把它替换成普通取消。
            error
        } else {
            request.cancel();
            assert_eq!(observer.first_errno(), None);
            assert!(doc.original_backing_health().is_ok());
            MarkdownTableRequestError::Cancelled
        };
        assert!(request.leaf_semantic.is_none() && request.semantic.is_none());
        assert!(request.batch.source_global_mut().is_err());
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).err(),
            Some(error)
        );
        drop(request);
        drop(doc);
        assert_eq!(observer.mapped_bytes(), 0);
        assert_eq!(observer.managed_resident_bytes(), 0);
    }
}

#[test]
/// 引用位于code之前时，真实定义/缺失先由同一Global闭合；code中的假闭标签不能截断外层下划线。
fn closed_root_reference_before_code_uses_verified_resolution_for_html_priority() {
    for prefix in ["", "## "] {
        for definitions in [
            "\n[R]: https://example.com/first\n[R]: https://example.com/later\n",
            "",
        ] {
            let source = format!("{prefix}<u>外层 [链接][R] `</u>` 仍然下划线</u>\n{definitions}");
            let (_directory, mut doc) = document(&source);
            doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
                .unwrap();
            let requested = 0..source.find('\n').unwrap().saturating_add(1);
            let requested = units(&source, requested);
            let mut request = doc
                .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
                .unwrap();
            finish_inline(&mut request, &doc).unwrap();
            let sources = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
                panic!("已验证引用上下文不得在code/HTML循环中停留Pending")
            };
            assert_oracle(ready, &doc, &source, &requested);
            let after = source.find("仍然").unwrap();
            let after = units(&source, after..after).start;
            assert!(
                ready
                    .query_for(&doc, authority(&doc), after)
                    .unwrap()
                    .unwrap()
                    .styles
                    .contains(MarkdownPresentationStyles::UNDERLINE)
            );
            let label = source.find("链接").unwrap();
            let label = units(&source, label..label).start;
            let link = ready.link_at_for(&doc, authority(&doc), label).unwrap();
            if definitions.is_empty() {
                assert!(link.is_none()); // 只有真实EOF才能证明这个query缺失；未收集label不在此豁免。
            } else {
                assert_eq!(
                    link.unwrap().destination.clone().into_string(),
                    "https://example.com/first"
                );
            }
        }
    }
}

/// 每个4096量子真实交还调用者；有界 Parser 的有限输入单独计量，不放宽旧零复制用例。
pub(super) fn finish_inline(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    for _ in 0..document.byte_len() * 100 + 10000 {
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(document, authority(document), 4096)?;
        assert!(work.source_bytes <= 4096);
        assert!(work.structural_transitions <= 4096);
        assert!(work.metadata_bytes <= 4096);
        assert!(work.parser_input_bytes <= 2048);
        assert!(work.copied_source_bytes <= 2048);
        assert!(work.allocated_source_capacity <= 2048);
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("真实 inline 请求没有在累计预算内结束")
}

#[test]
/// 新路径必须实际触发原 Parser，逐位置保留粗斜体、删除、代码、链接和中文/emoji 坐标。
fn closed_root_inline_is_production_ready_and_has_decoded_source_bound_links() {
    let source = "中文 **粗体**、*斜体*、~~删除~~、`代码😀`、[链接](https://example.com/a?x=1&amp;y=2) end\n";
    let (_directory, mut doc) = document(source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let requested = 0..doc.utf16_len();
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("短块必须进入完整 inline 生产路径，不能继续 Pending")
    };
    assert_oracle(ready, &doc, source, &requested);
    for (text, expected) in [
        ("粗体", MarkdownPresentationStyles::STRONG),
        ("斜体", MarkdownPresentationStyles::EMPHASIS),
        ("删除", MarkdownPresentationStyles::STRIKETHROUGH),
        ("代码", MarkdownPresentationStyles::INLINE_CODE),
        ("链接", MarkdownPresentationStyles::LINK),
    ] {
        let at = source.find(text).unwrap();
        let index = units(source, at..at).start;
        assert!(
            ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap()
                .styles
                .contains(expected)
        );
    }
    let at = source.find("链接").unwrap();
    let index = units(source, at..at).start;
    let link = ready
        .link_at_for(&doc, authority(&doc), index)
        .unwrap()
        .unwrap();
    assert_eq!(
        link.destination.clone().into_string(),
        "https://example.com/a?x=1&y=2"
    );
    assert_eq!(link.label_utf16_range, index..index + 2);
    assert!(
        ready
            .link_at_for(&doc, authority(&doc), 0)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        ready.link_at_for(&doc, authority(&doc), doc.utf16_len()),
        Err(MarkdownTableRequestError::Window(
            MarkdownTableWindowError::ContextPending
        ))
    );
    assert!(request.total_work().copied_source_bytes > 0);
    assert!(request.total_work().parser_input_bytes > 0);
    assert!(request.total_work().allocated_source_capacity > 0);
}

#[test]
/// 命中点的窄查询仍取得原闭合块的完整链接；后方整表不能成为打开首行链接的前置语义工作。
fn hit_point_reposition_keeps_complete_links_without_requesting_the_following_table() {
    let prefix = "未保存的中文😀前缀。\n\n[甲😀乙][目标] 和 [同块](../同块.md)\n\n";
    let mut source = String::from(prefix);
    source.push_str("| 第一列 | 第二列 | 第三列 | 第四列 |\n| --- | --- | --- | --- |\n");
    for row in 0..240 {
        use std::fmt::Write;
        writeln!(
            source,
            "| 第{row}行中文 | **真实粗体** | `真实代码😀` | 普通正文 |"
        )
        .unwrap();
    }
    // 引用定义在表后，窄窗口仍须按原Global解决完整引用，不能把窗口结束当作真实EOF。
    source.push_str("\n[目标]: https://example.com/a?x=1&amp;y=2 \"真实标题\"\n\n末尾独立正文。\n");
    let (directory, mut doc) = document(&source);
    let backing = doc.original_backing_observer().unwrap();
    doc.set_selection(MarkdownTextSelection::caret(prefix.encode_utf16().count()))
        .unwrap();
    let selection = doc.selection();
    let revision = doc.current_revision().unwrap();
    let label_byte = source.find("甲😀乙").unwrap();
    let label = units(&source, label_byte..label_byte + "甲😀乙".len());
    let other_label_byte = source.find("同块").unwrap();
    let other_label = units(&source, other_label_byte..other_label_byte + "同块".len());
    let table_byte = source.find("第一列").unwrap();
    let table_index = units(&source, table_byte..table_byte).start;
    let tail_byte = source.find("末尾独立正文").unwrap();
    let tail = units(&source, tail_byte..tail_byte + "末".len());
    let mut request = doc
        .viewport_leaf_source_request(tail, 57, selection)
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    for index in [label.start, label.start + 1, label.end - 1] {
        // 三次都沿同一个已发布SourceRequest重定位，既覆盖首/尾文字，也覆盖emoji的真实起点。
        // index+1只请求命中单位；Parser的闭合块和引用定义仍必须真实读取、解析和收费。
        request
            .reposition(&doc, &previous, index..index + 1, authority(&doc))
            .unwrap();
        finish_inline(&mut request, &doc).unwrap();
        previous = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = previous
            .ordinary_semantics_for(&doc, authority(&doc))
            .unwrap()
        else {
            panic!("真实命中点必须完成完整链接语义");
        };
        let link = ready
            .link_at_for(&doc, authority(&doc), index)
            .unwrap()
            .unwrap();
        assert_eq!(link.label_utf16_range, label);
        assert_eq!(
            link.destination.clone().into_string(),
            "https://example.com/a?x=1&y=2"
        );
        // 原有限Parser已完整证明同块另一个链接，正label事实不依赖当前1-unit样式覆盖。
        // 样式和无链接结论仍受原coverage限制，不能据此把后方整表或其它块冒充Ready。
        assert!(
            ready
                .query_for(&doc, authority(&doc), other_label.start)
                .unwrap()
                .is_none()
        );
        let other = ready
            .link_at_for(&doc, authority(&doc), other_label.start)
            .unwrap()
            .unwrap();
        assert_eq!(other.label_utf16_range, other_label);
        assert_eq!(other.destination.clone().into_string(), "../同块.md");
        assert_eq!(
            ready.link_at_for(&doc, authority(&doc), table_index),
            Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::ContextPending
            ))
        );
        let mut wrong_selection = authority(&doc);
        wrong_selection.selection = MarkdownTextSelection::caret(0);
        assert_eq!(
            ready.link_at_for(&doc, wrong_selection, other_label.start),
            Err(MarkdownTableRequestError::PublicationSuperseded)
        );
        assert_eq!(
            ready.link_at_for(&doc, authority(&doc), 0),
            Err(MarkdownTableRequestError::Window(
                MarkdownTableWindowError::ContextPending
            ))
        ); // 取得完整目标不扩大已发布的可查询范围，也不为窗口外文字补默认语义。
    }
    // 同字节文件仍是另一个真实Doc/Source；正链接不能绕过原根身份而跨文档续权。
    let (_other_directory, other_doc) = document(&source);
    let other_backing = other_doc.original_backing_observer().unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = previous
        .ordinary_semantics_for(&doc, authority(&doc))
        .unwrap()
    else {
        panic!("最后命中点必须仍持有原发布");
    };
    assert_eq!(
        ready.link_at_for(&other_doc, authority(&other_doc), other_label.start),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    drop(other_doc);
    assert_eq!(other_backing.mapped_bytes(), 0);
    assert_eq!(other_backing.managed_resident_bytes(), 0);
    assert_eq!(doc.selection(), selection);
    assert_eq!(doc.current_revision().unwrap(), revision);
    assert!(!doc.is_dirty() && !doc.can_undo() && !doc.can_redo());
    assert_eq!(
        std::fs::read(directory.path().join("有界叶块验收.md")).unwrap(),
        source.as_bytes()
    );
    drop(previous);
    drop(request);
    drop(doc);
    assert_eq!(backing.mapped_bytes(), 0);
    assert_eq!(backing.managed_resident_bytes(), 0);
    assert!(!backing.failed());
}

#[test]
/// 实际 label 内的嵌套括号、转义和 code 不截断可见文字；首尾原空白也不能被目标 marker 吞掉。
fn closed_root_links_preserve_real_label_boundaries_and_targets() {
    for label in [
        "a [b] c",
        r"a \] b",
        "a `]` b",
        "  a  ",
        "   ",
        "a\n   b  ",
        "a\n   ",
    ] {
        let source = format!("before [{label}](https://example.com/target) after\n");
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
            .unwrap();
        let requested = 0..doc.utf16_len();
        let mut request = doc
            .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
            .unwrap();
        finish_inline(&mut request, &doc).unwrap();
        let sources = request
            .take_sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap();
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
            panic!("真实合法链接必须进入已闭合 inline 生产路径: {label:?}")
        };
        assert_oracle(ready, &doc, &source, &requested);
        let start = "before [".len();
        for (relative, scalar) in label.char_indices() {
            if scalar == '`' {
                continue; // 真正 Code delimiter 自己仍由原 code owner 控制显隐，不能当普通 label。
            }
            let index = units(&source, start + relative..start + relative).start;
            let query = ready
                .query_for(&doc, authority(&doc), index)
                .unwrap()
                .unwrap();
            assert!(query.styles.contains(MarkdownPresentationStyles::LINK));
            assert!(
                !query.styles.contains(MarkdownPresentationStyles::MARKER),
                "label={label:?} scalar={scalar:?} index={index}"
            );
        }
        let close = units(&source, start + label.len()..start + label.len()).start;
        assert!(
            ready
                .query_for(&doc, authority(&doc), close)
                .unwrap()
                .unwrap()
                .styles
                .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        );
        assert!(
            ready
                .link_at_for(&doc, authority(&doc), close)
                .unwrap()
                .is_none()
        );
        let clickable = start + label.find('a').unwrap_or(0);
        let clickable = units(&source, clickable..clickable).start;
        let target = ready
            .link_at_for(&doc, authority(&doc), clickable)
            .unwrap()
            .unwrap();
        assert_eq!(
            target.destination.clone().into_string(),
            "https://example.com/target"
        );
        assert_eq!(
            target.label_utf16_range,
            units(&source, start..start + label.len())
        );
    }
}

#[test]
/// 引用定义在视口之前或之后都沿原 Global first-wins；不能用 block-only Parser 把它们判缺失。
fn closed_root_links_resolve_global_first_definition_and_preserve_old_publication() {
    let body = "正文 **粗体** 和 [链接][TARGET]。\n";
    let source = format!(
        "[target]: https://example.com/first\n\n{body}\n[target]: https://example.com/wrong\n"
    );
    let (_directory, doc) = document(&source);
    let start = source.find(body).unwrap();
    let requested = units(&source, start..start + body.len());
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let original_global = request.batch.source_probe_state().0;
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &previous.semantic else {
        panic!("真实全局引用必须完成生产语义")
    };
    assert_oracle(ready, &doc, &source, &requested);
    let at = source.find("链接").unwrap();
    let index = units(&source, at..at).start;
    assert_eq!(
        ready
            .link_at_for(&doc, authority(&doc), index)
            .unwrap()
            .unwrap()
            .destination
            .clone()
            .into_string(),
        "https://example.com/first"
    );
    assert_eq!(request.batch.source_probe_state().0, original_global);
    assert!(
        !request
            .batch
            .source_global_mut()
            .unwrap()
            .semantic_boundary_retained()
    );
    assert!(request.total_work().parser_input_bytes > 0);
}

#[test]
/// 真实 giant 单段落可以跨视口查到远处 closer，code 的假 HTML 不改变全局 underline 配对。
fn giant_inline_code_and_cross_block_underline_match_original_parser_without_copy() {
    let body = format!(
        "code prefix ` {} </u> 😀 end ` after\n",
        "中".repeat(12_000)
    );
    let source = format!("<u>outside open\n\n{body}\ntrue close</u> tail\n");
    let (_directory, mut doc) = document(&source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let body_start = source.find(&body).unwrap();
    let code_middle = units(&source, body_start..body_start).start + 6000;
    let requested = code_middle..code_middle + 31;
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &previous.semantic else {
        panic!("giant code 必须有真实 proof 与有限 Ready")
    };
    assert_oracle(ready, &doc, &source, &requested);
    let styles = ready
        .query_for(&doc, authority(&doc), requested.start)
        .unwrap()
        .unwrap()
        .styles;
    assert!(styles.contains(MarkdownPresentationStyles::INLINE_CODE));
    assert!(!styles.contains(MarkdownPresentationStyles::UNDERLINE)); // 原投影跨过 code 保留配对，但不在代码本体画下划线。
    let pointer = request.batch.source_probe_state().0;
    let next = code_middle + 20..code_middle + 55;
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(new) = &current.semantic else {
        panic!("前移不能丢失已证 giant code")
    };
    assert_oracle(new, &doc, &source, &next);
    assert_oracle(ready, &doc, &source, &requested);
    assert_eq!(request.batch.source_probe_state().0, pointer);
    let after_byte = body_start + body.find("after").unwrap();
    let after = units(&source, after_byte..after_byte + "after".len());
    request
        .continue_forward(&doc, &current, after.clone(), authority(&doc))
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let after_sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(after_ready) = &after_sources.semantic else {
        panic!("code 中假 closer 之后的普通正文仍须完成真实配对")
    };
    assert_oracle(after_ready, &doc, &source, &after);
    assert!(
        after_ready
            .query_for(&doc, authority(&doc), after.start)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
/// 短块原 Parser 的 code 事件实际交给同一 scanner，外部开标记不得被 code 中的假 closer 截断。
fn bounded_inline_code_cannot_close_an_external_underline() {
    let source = "<u>outside open\n\nshort `</u>` **粗体** after\n\ntrue close</u> tail\n";
    let (_directory, mut doc) = document(source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let body = "short `</u>` **粗体** after\n";
    let start = source.find(body).unwrap();
    let requested = units(source, start..start + body.len());
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("已闭合短块不能丢失真实 code/强调/跨块 underline")
    };
    assert_oracle(ready, &doc, source, &requested);
}

#[test]
/// 真 Root ATX 的小块复用完整 Parser；级别、缩进、闭合 #、选择显隐与 inline/目标共同接受差分。
fn short_root_atx_inline_uses_real_leaf_and_original_parser() {
    for level in 1..=6 {
        for ending in ["\n", "\r\n", ""] {
            let source = format!(
                "前言\n\n  {} 中文 **粗体** *斜体* ~~删除~~ `代码😀` [链接](https://example.com/atx) \\*字面 ##  {ending}",
                "#".repeat(level)
            );
            let requested = rich_heading_range(&source);
            for selection in [
                MarkdownTextSelection::caret(source.encode_utf16().count()),
                MarkdownTextSelection::caret(requested.start),
                MarkdownTextSelection {
                    anchor_utf16: requested.end,
                    head_utf16: requested.start,
                },
            ] {
                let (_directory, mut doc) = document(&source);
                doc.set_selection(selection).unwrap();
                let mut request = doc
                    .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
                    .unwrap();
                finish_inline(&mut request, &doc).unwrap();
                let sources = request
                    .take_sources_for(&doc, authority(&doc))
                    .unwrap()
                    .unwrap();
                let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
                    panic!("真实短 ATX 不得仍留 general-inline 空洞")
                };
                assert_oracle(ready, &doc, &source, &requested);
                let at = source.find("链接").unwrap();
                let at = units(&source, at..at).start;
                assert_eq!(
                    ready
                        .link_at_for(&doc, authority(&doc), at)
                        .unwrap()
                        .unwrap()
                        .destination
                        .clone()
                        .into_string(),
                    "https://example.com/atx"
                );
                assert!(request.total_work().copied_source_bytes > 0);
                assert!(request.total_work().parser_input_bytes > 0);
            }
        }
    }
}

#[test]
fn short_scoped_rich_atx_keeps_original_inline_references_prefixes_and_canonical_file() {
    for (prefix, text, ending) in [
        (
            "> ",
            "中文 **粗体** *斜体* `代码😀` [引用][T] &amp; $公式$ ###",
            "\r\n",
        ),
        ("> - ", "中文 **粗体** ~~删除~~ [引用][T] \\*字面 ###", "\n"),
        ("- > ", "中文 *斜体* `代码😀` [引用][T] &amp; ###", "\n"),
        ("> -\t", "中文 **粗体** [引用][T] ~下标~ ^上标^ ###", "\n"),
        ("> ", "中文 **粗体** [引用][T] {#标识 .样式}", "\n"),
    ] {
        let source = format!(
            "[T]: https://example.com/first\n[T]: https://example.com/later\n\n{prefix}## {text}{ending}\n尾\n"
        );
        let heading = rich_heading_range(&source);
        let physical_start = source.find(prefix).unwrap();
        let requested = units(&source, physical_start..source.len());
        let requested = requested.start..heading.end;
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(heading.start),
            MarkdownTextSelection {
                anchor_utf16: heading.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(&source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let mut request = doc
                .viewport_leaf_source_request(requested.clone(), 57, selection)
                .unwrap();
            finish_inline(&mut request, &doc).unwrap();
            let sources = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("真实短容器 ATX 必须走原标题入口，不能重判为 Root 或留 general 空洞");
            };
            assert_oracle(ready, &doc, &source, &requested);
            let label = source.find("引用").unwrap();
            let label = units(&source, label..label).start;
            assert_eq!(
                ready
                    .link_at_for(&doc, authority(&doc), label)
                    .unwrap()
                    .unwrap()
                    .destination
                    .clone()
                    .into_string(),
                "https://example.com/first"
            );
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
}

#[test]
/// 同一原 Global 的 ATX code 事件必须先排除假 HTML；代码后的普通标题文字仍继承外部配对。
fn short_atx_code_does_not_close_external_underline() {
    let source = "outside <u>open\n\n## code `</u>` **粗体** after\n\ntrue close</u> tail\n";
    let (_directory, mut doc) = document(source);
    doc.set_selection(MarkdownTextSelection::caret(doc.utf16_len()))
        .unwrap();
    let requested = rich_heading_range(source);
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("短 ATX 与外部 underline 必须沿真实 code 权限共同闭合")
    };
    assert_oracle(ready, &doc, source, &requested);
    let at = source.find("after").unwrap();
    let at = units(source, at..at).start;
    assert!(
        ready
            .query_for(&doc, authority(&doc), at)
            .unwrap()
            .unwrap()
            .styles
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );
}

#[test]
/// 真短容器段落和 Setext 共用原续行算法；独立完整 Parser 验每个源位置、链接及活动前缀。
fn short_scoped_paragraph_and_setext_preserve_original_inline_and_physical_prefixes() {
    for block in [
        "> 中文 **粗体** *斜体* `代码😀` [引用][T] &amp; $公式$\n> 第二行 ~~删除~~ 和 [行内](https://example.com/a?x=1&amp;y=2)\n",
        "> - 中文 **跨行\n>   粗体** 与 ` 代码\n>   \t续行😀 `、[引用][T] &amp;\n",
        "- > 中文 *跨行\n  > 斜体* 和 [引用][T]、~下标~ ^上标^\n",
        "> 中文 **第一行** [引用][T]\n没有引用标记的 lazy &amp; 续行😀\n",
        "> 中文 **Setext 标题** [引用][T]\n> ===\n",
        "> - 中文 *Setext* [引用][T]\n>   第二行 &amp; `代码😀`\n>   ---\n",
        "> 3. 中文 **有序列表** [引用][T]\r\n>    下一行 *斜体* &amp;\r\n",
        "> -\t中文 **Tab 列** [引用][T]\n> \t续行 `代码😀` &amp;\n",
        "> 中文 [引用][跨行\n> 标签] &amp; `代码`\n",
        "> 中文 **粗体** [引用][T] [跨行链接](\n> https://example.com/across\n> \"实际标题\") 和 ` 代码\r\n> \t跨行😀 ` &amp;\n",
    ] {
        let source = format!(
            "[T]: https://example.com/first\n[T]: https://example.com/later\n[跨行 标签]: https://example.com/first\n\n{block}\n尾\n"
        );
        let start = source.find(block).unwrap();
        let requested = units(&source, start..start + block.len());
        let label_byte = start + block.find("引用").unwrap();
        let label = units(&source, label_byte..label_byte).start;
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(label),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(&source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let mut request = doc
                .viewport_leaf_source_request(requested.clone(), 57, selection)
                .unwrap();
            finish_inline(&mut request, &doc).unwrap();
            let sources = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!(
                    "真实短 scoped rich/Setext 必须完成，不能用默认样式或 general 空洞冒充成功：{block:?}"
                );
            };
            assert_oracle(ready, &doc, &source, &requested);
            assert_eq!(
                ready
                    .link_at_for(&doc, authority(&doc), label)
                    .unwrap()
                    .unwrap()
                    .destination
                    .clone()
                    .into_string(),
                "https://example.com/first"
            );
            if let Some(at) = block.find("跨行链接") {
                let at = units(&source, start + at..start + at).start;
                assert_eq!(
                    ready
                        .link_at_for(&doc, authority(&doc), at)
                        .unwrap()
                        .unwrap()
                        .destination
                        .clone()
                        .into_string(),
                    "https://example.com/across",
                    "跨行 destination 不能把真实 quote prefix 当成 URL 文字"
                );
            }
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
}

#[test]
fn short_footnotes_preserve_real_numeric_labels_nested_openings_and_rich_children() {
    for block in [
        "[^123]: 中文 **粗体** *斜体* `代码😀` [链接][T] &amp;\n",
        "[^123]: 中文 **跨行\r\n    粗体** 和 ` 代码\r\n    \t跨行😀 ` [链接][T]\r\n",
        "> [^123]: 中文 **粗体** 和 [链接][T]\n>     续行 `代码😀` &amp;\n",
        "- [^123]: 中文 **粗体** 和 [链接][T]\n      续行 *斜体* &amp;\n",
        "[^123]: > 中文 **粗体** 和 [链接][T]\n    > 续行 `代码😀` &amp;\n",
        "[^外层]: > [^456]: 中文 **粗体** *斜体* `代码😀` [链接][T] &amp;\n",
        "> [^123]: > - 中文 **粗体** 和 [链接][T]\n>     >   续行 `代码😀` &amp;\n",
        "[^123]: 中文 **Setext** [链接][T]\n    ===\n",
        "[^123]: 中文 **EOF粗体** [链接][T]",
    ] {
        // 原引用定义仍在真实块之外，由同 Global 证明 first-wins；不能把 label 当 ordered marker，
        // 也不能把 footnote 的后继 quote/list 风格反向加给其字面 opening。
        let source =
            format!("[T]: https://example.com/first\n[T]: https://example.com/later\n\n{block}");
        let byte_start = source.find(block).unwrap();
        let requested = units(&source, byte_start..source.len());
        let label_byte = byte_start + block.find("链接").unwrap();
        let label = units(&source, label_byte..label_byte).start;
        for selection in [
            MarkdownTextSelection::caret(0),
            MarkdownTextSelection::caret(label),
            MarkdownTextSelection {
                anchor_utf16: requested.end,
                head_utf16: requested.start,
            },
        ] {
            let (directory, mut doc) = document(&source);
            let observer = doc.original_backing_observer().unwrap();
            doc.set_selection(selection).unwrap();
            let mut request = doc
                .viewport_leaf_source_request(requested.clone(), 57, selection)
                .unwrap();
            finish_inline(&mut request, &doc).unwrap();
            let sources = request
                .take_sources_for(&doc, authority(&doc))
                .unwrap()
                .unwrap();
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("短脚注必须消费原 opening/续行事实后完成，block={block:?}")
            };
            assert_oracle(ready, &doc, &source, &requested);
            assert_eq!(
                ready
                    .link_at_for(&doc, authority(&doc), label)
                    .unwrap()
                    .unwrap()
                    .destination
                    .clone()
                    .into_string(),
                "https://example.com/first"
            );
            // 真实 raw 值从原文档流式输出；标签、实体和未归一化 code 都必须原样保存，
            // 不因视觉前缀消费、反向选择或引用解析而产生磁盘写入/第二份正文。
            let mut actual = Vec::new();
            doc.write_utf16_range_to(requested.clone(), &mut actual)
                .unwrap();
            assert_eq!(actual, block.as_bytes());
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
}

#[test]
/// Setext 原 Fact 仍持 Paragraph self，完整小块包含真实下划线行；不能只按段落样式成功。
fn short_root_setext_rich_inline_preserves_real_heading_and_reference() {
    let source = "[target]: https://example.com/first\n\n中文 **粗体** *斜体* `代码` [链接][TARGET]\n===\n\n[target]: https://example.com/wrong\n";
    let (_directory, doc) = document(source);
    let requested = rich_heading_range(source);
    let mut request = doc
        .viewport_leaf_source_request(requested.clone(), 57, doc.selection())
        .unwrap();
    finish_inline(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("原 Setext self 范围中的小块 inline 必须实际完成")
    };
    assert_oracle(ready, &doc, source, &requested);
    let at = source.find("链接").unwrap();
    let at = units(source, at..at).start;
    assert_eq!(
        ready
            .link_at_for(&doc, authority(&doc), at)
            .unwrap()
            .unwrap()
            .destination
            .clone()
            .into_string(),
        "https://example.com/first"
    );
}

/// 请求范围来自独立完整 Parser 的真实 heading 事件，不能用待测工厂范围反造预期。
fn rich_heading_range(source: &str) -> Range<usize> {
    let range = Parser::new_ext(
        source,
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true),
    )
    .into_offset_iter()
    .find_map(|(event, range)| matches!(event, Event::Start(Tag::Heading { .. })).then_some(range))
    .unwrap();
    units(source, range)
}
