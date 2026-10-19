//! 从公开 Leaf 工厂进入原来源 owner，验证发布、续批和失效；不直接构造语义 job。

use super::*;

pub(super) fn finish_sources(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        // 真实最小量子逐次交还给调用方；没有函数内推进到 EOF 或 Ready 的生产捷径。
        let (MarkdownViewportSourceStep::Progress { work }
        | MarkdownViewportSourceStep::SourcesAvailable { work }) =
            request.advance(document, authority(document), 4096)?;
        assert_step(work);
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("公开 Leaf 来源请求未在有限累计预算内结束");
}

pub(super) fn leaf_sources(
    document: &MarkdownDocumentBuffer,
    requested: Range<usize>,
) -> (
    MarkdownViewportSourceRequest,
    VerifiedMarkdownViewportSources,
) {
    let mut request = document
        .viewport_leaf_source_request(requested, 57, document.selection())
        .unwrap();
    finish_sources(&mut request, document).unwrap();
    let sources = request
        .take_sources_for(document, authority(document))
        .unwrap()
        .unwrap();
    (request, sources)
}

#[test]
fn production_leaf_factory_uses_original_publication_and_matches_complete_projection() {
    for source in [
        "前言\n\n```rust\n中文😀<u>代码</u>\n```\n\n结尾\n",
        "前言\r\n\r\n  ~~~~text\r\n中文😀\r\n  ~~~~~ \r\n\r\n结尾\r\n",
        "前言\n\n    中文😀\n    第二行\n\n结尾\n",
        "前言\n\n  ***  \n\n结尾\n",
        "前言\n\n```text\n真实EOF中文😀",
        "<u>远处开标记\n\n```text\n中文😀\n```\n\n远处闭标记</u>\n",
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
            let (request, mut sources) = leaf_sources(&doc, requested.clone());
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
                .ordinary_semantics_for(&doc, authority(&doc))
                .unwrap()
            else {
                panic!("公开工厂的真实 Root 代码块或分隔线必须产生有限语义");
            };
            assert_oracle(ready, &doc, source, &requested);
            assert!(
                sources
                    .records
                    .iter()
                    .any(|record| Rc::ptr_eq(record, &ready.coverage.get(0).unwrap().record))
            );
            assert!(request.leaf_semantic.is_none());
            assert!(request.semantic.is_none());
            assert!(request.records.is_empty());
            assert_eq!(request.total_work().copied_source_bytes, 0);
            assert_eq!(request.total_work().parser_input_bytes, 0);
            assert_eq!(request.total_work().allocated_source_capacity, 0);

            // 取出实际 Ready 后结束原 SourcePublication，孤立的语义不能重新借相同 g 取得权限。
            let state = std::mem::replace(
                &mut sources.semantic,
                MarkdownOrdinarySemanticState::SourcesOnly,
            );
            let MarkdownOrdinarySemanticState::RootLeafReady(ready) = state else {
                unreachable!();
            };
            drop(sources);
            assert_eq!(
                ready.query_for(&doc, authority(&doc), requested.start),
                Err(MarkdownTableRequestError::PublicationSuperseded)
            );
        }
    }
}

#[test]
fn production_leaf_giant_line_has_finite_runs_without_copy_or_parser_input() {
    let source = format!("前言\n\n```text\n{}\n```\n\n尾\n", "中".repeat(100_000));
    let (_directory, doc) = document(&source);
    let requested = 50_000..50_017;
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("巨型单行不能因为整个块超过窗口大小而失去有限代码语义");
    };
    assert_eq!(
        ready.coverage_for(&doc, authority(&doc)).unwrap(),
        &requested
    );
    assert_eq!(ready.run_count(), 1);
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
    assert!(request.leaf_semantic.is_none());
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn production_leaf_forward_charges_old_pages_and_runs_without_retaining_old_publication() {
    let source = format!(
        "前言\n\n```text\n{}\n```\n\n尾\n",
        "<u>中文😀</u>".repeat(160)
    );
    let (_directory, doc) = document(&source);
    let block = units(&source, first_leaf_range(&source));
    let first_range = block.start + 50..block.start + 700;
    let (mut request, previous) = leaf_sources(&doc, first_range.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("旧来源必须有真实页和 run");
    };
    assert!(old.run_count() > PAGE_RUNS);
    let actual_metadata = previous.records.len() * record_metadata()
        + publication_metadata()
        + old.retained_metadata()
        + previous.blocks.len() * block_metadata()
        + previous.blocks.capacity() * std::mem::size_of::<Rc<OrdinaryBlockRecord>>();
    assert_eq!(previous.retained_metadata, actual_metadata);
    let old_runs = old.run_count();
    let old_publication = Rc::downgrade(&previous.publication);
    let global = request.batch.source_probe_state().0;
    assert_ne!(
        global, 0,
        "必须实际保留原结构前沿，两个空槽不能证明同一 Global"
    );
    let next = block.start + 100..block.start + 900;
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    assert_eq!(quota.remaining_runs(), 8192 - old_runs);
    assert!(
        quota.metadata_used()
            >= previous.retained_metadata
                + source_workspace_metadata()
                + publication_reservation_metadata()
    );
    finish_sources(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, global);
    assert!(request.leaf_semantic.is_none());
    let next_sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &next_sources.semantic else {
        panic!("原 Global 续批必须完成同源代码交集");
    };
    assert_oracle(ready, &doc, &source, &next);
    assert_oracle(old, &doc, &source, &first_range);
    // 后继只共享原 immutable Leaf 记录；不保父 SourcePublication 链或旧 Ready 的页。
    drop(previous);
    assert!(old_publication.upgrade().is_none());
    assert!(
        ready
            .query_for(&doc, authority(&doc), next.start)
            .unwrap()
            .is_some()
    );
}

#[test]
fn production_leaf_forward_quota_failure_preserves_old_ready_and_releases_new_owner() {
    let source = "前言\n\n```text\n中文😀正文\n```\n\n尾\n";
    let (_directory, doc) = document(source);
    let requested = units(source, first_leaf_range(source));
    let (mut request, previous) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("旧来源必须先取得真正 Ready");
    };
    request
        .continue_forward(
            &doc,
            &previous,
            requested.start + 1..requested.end,
            authority(&doc),
        )
        .unwrap();
    let quota = request.batch.source_semantic_quota().unwrap();
    request
        .batch
        .admit_source_runs(quota.remaining_runs())
        .unwrap();
    let expected = MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit);
    assert_eq!(finish_sources(&mut request, &doc), Err(expected));
    assert!(request.leaf_semantic.is_none());
    assert!(request.records.is_empty());
    assert!(request.ready.is_none());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(expected)
    );
    assert!(
        old.query_for(&doc, authority(&doc), requested.start)
            .unwrap()
            .is_some()
    );
}

#[test]
fn production_leaf_edit_undo_and_cancel_cannot_reauthorize_same_root_or_keep_document_alive() {
    let source = "前言\n\n```text\n中文😀正文\n```\n\n尾\n";
    let (_directory, mut doc) = document(source);
    let requested = units(source, first_leaf_range(source));
    let (mut request, previous) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &previous.semantic else {
        panic!("原发布必须有真正 Ready");
    };
    let root = std::sync::Arc::downgrade(doc.root.as_ref().unwrap());
    let generation = doc.generation();
    doc.insert_utf16(0, "插入", MarkdownTextSelection::caret(2))
        .unwrap();
    doc.undo().unwrap();
    assert!(std::sync::Weak::ptr_eq(
        &root,
        &std::sync::Arc::downgrade(doc.root.as_ref().unwrap())
    ));
    assert!(doc.generation() > generation);
    assert_eq!(
        ready.query_for(&doc, authority(&doc), requested.start),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::SourceSuperseded)
    );
    assert!(request.leaf_semantic.is_none());
    drop(doc);
    assert_eq!(
        root.strong_count(),
        0,
        "请求和外带 Ready 都不能强持 piece root/mmap"
    );

    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_leaf_source_request(requested, 57, doc.selection())
        .unwrap();
    request.advance(&doc, authority(&doc), 4096).unwrap();
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        request.advance(&doc, cancelled, 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(request.leaf_semantic.is_none());
    assert!(request.records.is_empty());
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
}

#[test]
fn production_leaf_unknown_scope_inline_and_false_eof_never_become_ready() {
    for (source, whole_document, expected) in [
        (
            "> ```\n> 中文\n> ```\n\n尾\n",
            false,
            MarkdownRootLeafPendingReason::NonRootScope,
        ),
        (
            "````\n正文\n```",
            false,
            MarkdownRootLeafPendingReason::LegacyFenceBoundaryMismatch,
        ),
        (
            "```\n正文\n~~~",
            false,
            MarkdownRootLeafPendingReason::LegacyFenceBoundaryMismatch,
        ),
        (
            "```\n正文\n```不是closer",
            false,
            MarkdownRootLeafPendingReason::LegacyFenceBoundaryMismatch,
        ),
        (
            "只有**复杂行内正文**，没有已证代码或分隔线\n",
            true,
            MarkdownRootLeafPendingReason::NeedsGeneralInline,
        ),
    ] {
        let (_directory, doc) = document(source);
        let requested = if whole_document {
            0..doc.utf16_len()
        } else {
            units(source, first_leaf_range(source))
        };
        let (request, sources) = leaf_sources(&doc, requested);
        assert!(
            matches!(&sources.semantic, MarkdownOrdinarySemanticState::LeafPending(reason) if *reason == expected)
        );
        assert!(request.leaf_semantic.is_none());
        assert_eq!(sources.semantic.run_count(), 0);
        assert_eq!(sources.semantic.retained_metadata(), 0);
    }
    // 原 SourcesOnly 工厂仍只提供来源事实，不因加入 Leaf 模式而偷偷升级任何语义。
    let source = "```\n正文\n```\n";
    let (_directory, doc) = document(source);
    let (_, sources) = source_request(&doc, 0..doc.utf16_len());
    assert!(matches!(
        sources.semantic,
        MarkdownOrdinarySemanticState::SourcesOnly
    ));
}

#[test]
fn production_joint_plain_blocks_keep_exact_ranges_and_original_leaf_identity() {
    let first = "中文普通段落😀。\r\n第二行仍是普通文本。\r\n";
    let setext = "第二个普通闭合块😀\n=====\n";
    let last = "末段中文与 emoji 😀。\n";
    let source = format!(
        "# 标题\n\n{first}\r\n{setext}\n**复杂正文仍未知**\n\n```text\n代码\n```\n\n{last}"
    );
    let (_directory, doc) = document(&source);
    let requested = 0..doc.utf16_len();
    let (request, sources) = leaf_sources(&doc, requested.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &sources.semantic else {
        panic!("同一真实来源必须发布普通块与 Leaf 的已证交集");
    };
    let mut baseline = MarkdownViewportPresentation::default();
    baseline.set_active_selection(ordered_selection(doc.selection()));
    baseline
        .rebuild(doc.generation(), requested, &source)
        .unwrap();
    for text in [first, setext, last] {
        let start = source.find(text).unwrap();
        let range = units(&source, start..start + text.len());
        assert!(
            ready
                .covered_ranges_for(&doc, authority(&doc))
                .unwrap()
                .any(|actual| actual == &range)
        );
        for index in range {
            assert_eq!(
                ready
                    .query_for(&doc, authority(&doc), index)
                    .unwrap()
                    .unwrap()
                    .styles,
                baseline.query(index).unwrap().styles
            );
            assert!(
                ready
                    .block_kind_at_for(&doc, authority(&doc), index)
                    .unwrap()
                    .is_none(),
                "真正普通块不能冒充 LeafBlockKind"
            );
        }
    }
    let unknown = source.find("**复杂").unwrap();
    assert!(
        ready
            .query_for(
                &doc,
                authority(&doc),
                units(&source, unknown..unknown).start
            )
            .unwrap()
            .is_none()
    );
    assert!(request.leaf_semantic.is_none());
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn production_joint_giant_plain_forward_uses_same_global_and_never_copies_paragraph() {
    let source = format!("{}\n", "中文😀".repeat(30_000));
    let (_directory, doc) = document(&source);
    let first = 50_000..50_019;
    let (mut request, previous) = leaf_sources(&doc, first.clone());
    let MarkdownOrdinarySemanticState::RootLeafReady(old) = &previous.semantic else {
        panic!("巨型单段落必须提供有限普通语义");
    };
    assert_oracle(old, &doc, &source, &first);
    let original_global = request.batch.source_probe_state().0;
    assert_ne!(original_global, 0);
    let next = 50_010..50_031;
    request
        .continue_forward(&doc, &previous, next.clone(), authority(&doc))
        .unwrap();
    finish_sources(&mut request, &doc).unwrap();
    assert_eq!(request.batch.source_probe_state().0, original_global);
    let current = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let MarkdownOrdinarySemanticState::RootLeafReady(ready) = &current.semantic else {
        panic!("原 seed/Global 续批不能丢失巨型段落的有限交集");
    };
    assert_oracle(ready, &doc, &source, &next);
    assert_oracle(old, &doc, &source, &first);
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().parser_input_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
}

#[test]
fn production_joint_active_plain_cancel_and_run_limit_release_real_scratch() {
    for cancel in [true, false] {
        let (_directory, doc) = document("中文闭合普通段落😀。\n");
        let mut request = doc
            .viewport_leaf_source_request(0..doc.utf16_len(), 57, doc.selection())
            .unwrap();
        // 等到真实 Plain scratch 接管原 underline 后再注入业务取消或联合库存耗尽。
        for _ in 0..10_000 {
            if request.semantic.is_some() {
                break;
            }
            request.advance(&doc, authority(&doc), 4096).unwrap();
            assert!(
                request.ready.is_none(),
                "普通块未证明时不能提前发布默认样式"
            );
        }
        assert!(request.semantic.is_some());
        let expected = if cancel {
            request.cancel();
            MarkdownTableRequestError::Cancelled
        } else {
            let quota = request.batch.source_semantic_quota().unwrap();
            quota.admit_retained(quota.remaining_runs(), 0).unwrap();
            MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit)
        };
        assert_eq!(finish_sources(&mut request, &doc), Err(expected));
        assert!(request.leaf_semantic.is_none());
        assert!(request.records.is_empty());
        assert!(request.blocks.is_empty());
        assert!(request.ready.is_none());
        assert_eq!(request.advance(&doc, authority(&doc), 4096), Err(expected));
    }
}

#[test]
/// 一次冷准备可在原量子装下短端点；surrogate内的视口末端仍沿真实scalar向上取整。
fn joint_bounds_coalesce_short_endpoints_inside_original_quantum() {
    let source = "中文😀普通段落。\n";
    let (_directory, mut doc) = document(source);
    doc.set_selection(MarkdownTextSelection {
        anchor_utf16: 8,
        head_utf16: 9,
    })
    .unwrap();
    let mut request = doc
        .viewport_leaf_source_request(2..3, 57, doc.selection())
        .unwrap();
    let MarkdownViewportSourceStep::Progress { work } =
        request.advance(&doc, authority(&doc), 4096).unwrap()
    else {
        panic!("坐标齐备不等于语义或来源已经发布");
    };
    assert_step(work);
    assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
    let job = request.leaf_semantic.as_ref().unwrap();
    assert!(!job.preparing());
    assert_eq!(job.position_index, 4);
    assert_eq!(job.byte_coverage, Some(6..10));
    assert_eq!(job.selection_bytes, 22..25);
    assert!(request.ready.is_none());
    assert_eq!(
        request.batch.source_probe_state().1,
        0,
        "原量子只完成四端点，不顺带推进Global"
    );
}

#[test]
/// 远端片内前缀仍可跨量子，真实byte进度/Source错误保留，不为冷显示扩大读窗或预算。
fn joint_bounds_long_prefix_remains_pending_and_reclaims_source_errors() {
    for fault in [false, true] {
        let source = format!("{}\n", "a".repeat(4000));
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 3000,
            head_utf16: 3001,
        })
        .unwrap();
        let mut request = doc
            .viewport_leaf_source_request(0..2, 57, doc.selection())
            .unwrap();
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("未完成的远端前缀不能发布来源");
        };
        assert_step(work);
        assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        assert!(request.leaf_semantic.as_ref().unwrap().preparing());
        if fault {
            // 原Bounds先真实读入同页并保留未完成端点；归还pin不会驱逐缓存页，不能把注入标志当成I/O失败。
            let observer = doc.original_backing_observer().unwrap();
            let attempts = observer.reclaim_attempts();
            let successes = observer.reclaim_successes();
            doc.fail_next_original_reclaim_for_test();
            assert_eq!(
                doc.reclaim_original_read_windows_for_test(),
                Err(crate::MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO))
            );
            assert!(observer.reclaim_attempts() > attempts);
            assert!(observer.reclaim_successes() > successes);
            assert_eq!(observer.first_errno(), Some(libc::EIO));
            // 量子之间的真实回收失败先撤销原Source健康权限；下次入口必须固定SourceSuperseded并释放未发布资源。
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096).err(),
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            assert_eq!(
                request.failed,
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            assert!(request.leaf_semantic.is_none());
            assert!(request.semantic.is_none());
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096).err(),
                Some(MarkdownTableRequestError::SourceSuperseded)
            );
            continue; // 真正读窗归还失败固定首因，不能用同坐标重试或发布半份来源。
        }
        for _ in 0..100 {
            if !request.leaf_semantic.as_ref().unwrap().preparing() {
                break;
            }
            let MarkdownViewportSourceStep::Progress { work } =
                request.advance(&doc, authority(&doc), 4096).unwrap()
            else {
                panic!("Bounds不取得后继结构或语义的发布权限");
            };
            assert_step(work);
            assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
            assert!(work.structural_transitions > 0);
        }
        let job = request.leaf_semantic.as_ref().unwrap();
        assert!(!job.preparing());
        assert_eq!(job.byte_coverage, Some(0..2));
        assert_eq!(job.selection_bytes, 3000..3001);
        assert_eq!(request.batch.source_probe_state().1, 0);
    }
}

#[test]
/// 真实长源的窗口端点与远选区各在自己的小片内续用；源工作少于四次独立前缀解码，不读取两端之间正文。
fn joint_bounds_reuse_actual_piece_prefixes_with_far_selection_and_preserve_failure() {
    let source = "a".repeat(100_000);
    for fault in [false, true] {
        let (_directory, mut doc) = document(&source);
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 31215,
            head_utf16: 31226,
        })
        .unwrap();
        let mut request = doc
            .viewport_leaf_source_request(161..501, 57, doc.selection())
            .unwrap();
        let MarkdownViewportSourceStep::Progress { work } =
            request.advance(&doc, authority(&doc), 4096).unwrap()
        else {
            panic!("有限端点不能提前发布结构和语义");
        };
        assert_step(work);
        assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        assert!(request.leaf_semantic.as_ref().unwrap().preparing());
        if fault {
            // 两端点仍归同一可取消请求；取消后不能用已认证小片标量恢复任何读取或发布。
            request.cancel();
            assert_eq!(request.failed, Some(MarkdownTableRequestError::Cancelled));
            assert!(request.leaf_semantic.is_none());
            assert!(request.semantic.is_none());
            assert_eq!(
                request.advance(&doc, authority(&doc), 4096),
                Err(MarkdownTableRequestError::Cancelled)
            );
            continue;
        }
        for _ in 0..100 {
            if !request.leaf_semantic.as_ref().unwrap().preparing() {
                break;
            }
            let MarkdownViewportSourceStep::Progress { work } =
                request.advance(&doc, authority(&doc), 4096).unwrap()
            else {
                panic!("Bounds只交付端点，不前移Global或发布Ready");
            };
            assert_step(work);
            assert!(work.source_bytes + work.structural_transitions + work.metadata_bytes <= 4096);
        }
        let job = request.leaf_semantic.as_ref().unwrap();
        assert!(!job.preparing());
        assert_eq!(job.byte_coverage, Some(161..501));
        assert_eq!(job.selection_bytes, 31215..31226);
        assert_eq!(request.batch.source_probe_state().1, 0);
        let mut independent_bytes = 0;
        for (target, ceil) in [(161, false), (501, true), (31215, false), (31226, true)] {
            let mut locator = PositionLocator::new(target, ceil);
            for _ in 0..100 {
                if locator.result().is_some() {
                    break;
                }
                independent_bytes += locator.advance(&doc, 4096).unwrap().source_bytes;
            }
            assert_eq!(locator.result(), Some(target));
        }
        assert!(request.total_work().source_bytes < independent_bytes);
        assert_eq!(request.total_work().source_bytes, 501 + (31226 - 7 * 4096));
        assert_eq!(request.total_work().copied_source_bytes, 0);
        assert_eq!(request.total_work().parser_input_bytes, 0);
    }
}
