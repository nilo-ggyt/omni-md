//! 原文件已有长格的真实激活、失败隔离与保存历史；不手造语义窗口或整格编辑副本。

use super::*;

/// 实际结构请求再流式取格端点；纯测试循环不参与产品事件调度。
pub(super) fn plan(
    doc: &MarkdownDocumentBuffer,
    row: usize,
    column: usize,
) -> MarkdownTableCellEditPlan {
    let mut request = doc.table_cell_edit_request(0, row, column).unwrap();
    loop {
        match request.advance(doc, false, 4096).unwrap() {
            MarkdownTableCellEditStep::Progress { .. } => {}
            MarkdownTableCellEditStep::Ready { .. } => {
                return request.take_plan_for(doc).unwrap().unwrap();
            }
            other => panic!("真实原格没有编辑计划：{other:?}"),
        }
    }
}

#[test]
/// 流式词法与原有限Parser在同一真实源上给出相同替换及缺格前缀；不维护另一份GFM oracle。
fn streamed_cells_match_original_window_plans_for_escapes_padding_and_missing_cells() {
    for prefix in ["", "> > ", "   "] {
        for row in [
            "| a | 中😀 |",
            "|a||",
            "a\\|b",
            "a\\\\|",
            "`a|b`",
            "a",
            "|a|  ",
            "| a\u{b} | b\u{c} |",
        ] {
            let source = format!(
                "{prefix}| A | B | C | D |\r\n{prefix}| - | - | - | - |\r\n{prefix}{row}\r\n"
            );
            let (_directory, doc) = document(&source);
            let verified = window(&doc);
            for column in 0..4 {
                // 原正式短格准备者和新原格请求必须最终消费同一个plan职责。
                let expected = doc
                    .plan_verified_table_cell_edit(&verified, 1, column, "")
                    .unwrap();
                let actual = plan(&doc, 1, column);
                assert_eq!(
                    actual.replacement_for(&doc).unwrap(),
                    expected.replacement_for(&doc).unwrap(),
                    "{source:?}, column={column}"
                );
            }
        }
    }
}

#[test]
/// 原公开替换计划可带正文；激活不能悄悄将它保成补格前缀或发布文本。
fn nonempty_explicit_replacement_plan_is_rejected_before_creating_a_draft() {
    let (_directory, mut doc) = document(SOURCE);
    let verified = window(&doc);
    let replacement = doc
        .plan_verified_table_cell_edit(&verified, 1, 0, "替换正文")
        .unwrap();
    let before = doc.storage_profile();
    assert!(matches!(
        doc.begin_prepared_cell_draft(replacement),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::InvalidSource
        ))
    ));
    assert_eq!(doc.storage_profile(), before);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(!doc.has_cell_draft());
    let empty = plan(&doc, 1, 0);
    let token = doc.begin_prepared_cell_draft(empty).unwrap();
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
/// 原2MiB格从文件映射切出；两份真实Reader不阻止准备，归还后保存和Undo/Redo只发布一次。
fn existing_two_mib_cell_edits_and_saves_without_whole_cell_materialization() {
    let mut payload = "x".repeat(2 * 1024 * 1024 - "中😀".len());
    payload.push_str("中😀");
    let source = format!("| H | C |\n| --- | --- |\n| {payload} | other |\n");
    let (directory, original) = document(&source);
    drop(original);
    let workspace = crate::MarkdownWorkspace::new(
        directory.path().to_path_buf(),
        directory.path().join("app-data"),
    );
    let access = workspace.open_session().unwrap();
    let mut doc = access.open_document_buffer("cell-draft.md").unwrap();
    let prepared = plan(&doc, 1, 0);
    let token = doc.begin_prepared_cell_draft(prepared).unwrap();
    let facts = doc.cell_draft_facts(&token).unwrap();
    assert_eq!(facts.byte_len, 2 * 1024 * 1024);
    assert_eq!(doc.storage_profile().allocated_edit_segment_bytes, 0);
    edit(
        &mut doc,
        &token,
        facts.utf16_len..facts.utf16_len,
        "尾",
        MarkdownTextSelection::caret(facts.utf16_len + 1),
        None,
    )
    .unwrap();
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let mut tail = [0; 4];
    first
        .copy_utf16_code_units(&first.authority(), facts.utf16_len - 3, &mut tail)
        .unwrap();
    assert_eq!(String::from_utf16(&tail).unwrap(), "中😀尾");
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert!(candidate.unmap_succeeded());
    drop((first, second)); // 同一原读取值真正归还后重新准备，不能重复消费旧准入票。
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    let mapping = prepared.replacement_backing_observer().unwrap();
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    let expected = source.replacen("中😀 |", "中😀尾 |", 1);
    access.save_document_buffer(&mut doc).unwrap();
    assert_eq!(std::fs::read(doc.path()).unwrap(), expected.as_bytes());
    assert_eq!(doc.storage_profile().undo_entries, 1);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), source);
    assert!(doc.is_dirty());
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    assert!(!doc.is_dirty());
    drop(doc);
    assert!(mapping.unmap_succeeded());
}

#[test]
/// 文档释放票覆盖真正发布的文件段及其历史；准备候选与raw分支仍归各自事务。
fn document_backing_observers_follow_published_file_through_undo_redo_and_drop() {
    let (_directory, mut doc) = document(SOURCE);
    let original = doc.original_backing_observer().unwrap();
    let token = begin(&mut doc);
    let text = "x".repeat(80_000);
    replace(&mut doc, &token, &text);
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    let candidate = prepared.replacement_backing_observer().unwrap();
    // 两个尚未发布的实际文件都不能成为canonical文档的释放责任。
    assert!(
        doc.backing_observers()
            .all(|backing| backing.mapped_bytes() == SOURCE.len())
    );
    doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    let expected = SOURCE.replacen("| a |", &format!("| {text} |"), 1);
    assert_eq!(canonical(&doc), expected);
    let published = doc
        .backing_observers()
        .find(|backing| backing.mapped_bytes() == text.len())
        .expect("实际发布的文件段必须进入文档最终释放票");
    assert!(!candidate.unmap_succeeded());
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    assert!(
        doc.backing_observers()
            .any(|backing| backing.mapped_bytes() == text.len())
    );
    assert!(!published.unmap_succeeded()); // Undo保留真实Redo正文，不能提前报告释放。
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    let backings: Vec<_> = doc.backing_observers().collect();
    drop(doc);
    // observer与弱token都不延长映射寿命；成功结论来自每个实际munmap结果。
    for backing in backings {
        assert!(backing.unmap_succeeded());
        assert_eq!(backing.mapped_bytes(), 0);
        assert!(!backing.failed());
    }
    assert!(original.unmap_succeeded());
    assert!(candidate.unmap_succeeded());
    assert!(published.unmap_succeeded());
}

#[test]
/// 取消、另一Doc、旧代次与实际源EIO都撤销原请求；无正文请求不保活映射。
fn cancelled_foreign_and_failed_original_requests_never_issue_an_edit_plan() {
    let (_directory, mut doc) = document(SOURCE);
    let mut cancelled = doc.table_cell_edit_request(0, 1, 0).unwrap();
    cancelled.advance(&doc, false, 4096).unwrap();
    assert_eq!(
        cancelled.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Cancelled)
    );
    assert!(cancelled.take_plan_for(&doc).is_err());
    let (_other_directory, other) = document(SOURCE);
    let mut foreign = doc.table_cell_edit_request(0, 1, 0).unwrap();
    assert_eq!(
        foreign.advance(&other, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    let original = plan(&doc, 1, 0);
    let (_third_directory, mut third) = document(SOURCE);
    assert!(third.begin_prepared_cell_draft(original).is_err());
    assert_eq!(canonical(&third), SOURCE);
    assert!(!third.has_cell_draft());
    let mut stale = doc.table_cell_edit_request(0, 1, 0).unwrap();
    doc.insert_utf16(0, "新", MarkdownTextSelection::caret(1))
        .unwrap();
    assert_eq!(
        stale.advance(&doc, false, 4096),
        Err(MarkdownTableRequestError::SourceSuperseded)
    );
    doc.undo_text_edit().unwrap();
    let mut failed = doc.table_cell_edit_request(0, 1, 0).unwrap();
    doc.copy_utf16_code_units(0, &mut [0]).unwrap();
    doc.original_snapshot
        .as_ref()
        .unwrap()
        .fail_next_reclaim_for_test();
    assert!(doc.reclaim_original_read_windows_for_test().is_err());
    assert_eq!(
        failed.advance(&doc, true, 4096),
        Err(MarkdownTableRequestError::Source(
            MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO)
        ))
    );
    let (_live_directory, live) = document(SOURCE);
    let mut request = live.table_cell_edit_request(0, 1, 0).unwrap();
    request.advance(&live, false, 4096).unwrap();
    let backing = live.original_backing_observer().unwrap();
    drop(live);
    assert!(backing.unmap_succeeded());
    drop(request);
}

#[test]
/// 巨型raw最终只剩小结果时释放匿名文件，有限Inline仍沿一次提交与历史恢复。
fn long_trimmed_input_releases_file_before_retaining_a_small_inline_result() {
    let (_directory, mut doc) = document(SOURCE);
    let token = begin(&mut doc);
    let raw = format!("中😀{}", " ".repeat(2 * 1024 * 1024));
    replace(&mut doc, &token, &raw);
    let facts = doc.cell_draft_facts(&token).unwrap();
    doc.set_cell_draft_selection(
        doc.capture_cell_draft_edit(&token).unwrap(),
        MarkdownTextSelection {
            anchor_utf16: facts.utf16_len,
            head_utf16: 0,
        },
    )
    .unwrap();
    let prepared = doc
        .prepare_cell_draft_commit(
            doc.capture_cell_draft_edit(&token).unwrap(),
            Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: facts.utf16_len - 1,
                    head_utf16: 0,
                },
                origin_utf16: facts.utf16_len - 1,
            }),
        )
        .unwrap();
    assert!(prepared.replacement_backing_observer().is_none());
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    assert!(observer.snapshot().prepared_commit_bytes <= EDIT_SEGMENT_BYTES);
    let committed = doc
        .commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap();
    assert!(committed.edit.is_some());
    // 两MiB尾空白确实已截去；反向选择的旧末端落在留下的“中😀”之后，不保整格映射表。
    assert_eq!(
        committed.receipt.selection,
        MarkdownTextSelection {
            anchor_utf16: facts.canonical_range.start + 3,
            head_utf16: facts.canonical_range.start,
        }
    );
    assert_eq!(
        committed.receipt.word_anchor,
        Some(MarkdownCellDraftWordAnchor {
            range: MarkdownTextSelection {
                anchor_utf16: facts.canonical_range.start + 3,
                head_utf16: facts.canonical_range.start
            },
            origin_utf16: facts.canonical_range.start + 3
        })
    );
    let expected = SOURCE.replacen("| a |", "| 中😀 |", 1);
    assert_eq!(canonical(&doc), expected);
    doc.undo_text_edit().unwrap();
    assert_eq!(canonical(&doc), SOURCE);
    doc.redo_text_edit().unwrap();
    assert_eq!(canonical(&doc), expected);
    assert!(observer.is_released());
}

/// 真实 RootLeaf 请求先发布结构行，再沿同一个 owner 完成 CellInline 和显示目录。
/// 只交回原 Parser 的格票；实际显示 Source/匿名目录均在激活之前归还，不保留另一张读图。
#[cfg(feature = "paged-line-compose")]
fn displayed_cell(
    document: &MarkdownDocumentBuffer,
    row_start: usize,
    column: usize,
) -> VerifiedMarkdownTableCellSource {
    use crate::{MarkdownTableCellDisplayPreparation, MarkdownTableCellDisplayPreparationStep};
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 92,
        selection: document.selection(),
        cancelled: false,
    };
    let mut request = document
        .viewport_leaf_source_request(
            0..document.utf16_len().min(128),
            authority.context_revision,
            authority.selection,
        )
        .unwrap();
    let publication = loop {
        request.advance(document, authority, 4096).unwrap();
        if let Some(publication) = request.take_sources_for(document, authority).unwrap() {
            break publication;
        }
    };
    let row = publication
        .table_row_at_for(document, authority, row_start)
        .unwrap()
        .unwrap();
    let mut preparation =
        MarkdownTableCellDisplayPreparation::from_row_source(document, row, authority, column)
            .unwrap();
    let display = loop {
        let step = preparation
            .advance_with_source_request(document, &mut request, false)
            .unwrap();
        let (ready, work) = match step {
            MarkdownTableCellDisplayPreparationStep::Progress { work } => (false, work),
            MarkdownTableCellDisplayPreparationStep::Ready { work } => (true, work),
        };
        // 原 Parser 和转换器各步都须真实遵守公开输入量子；巨格不能在一个步骤中暗读全文。
        assert!(work.source_bytes <= 4096);
        assert!(work.copied_source_bytes <= 4096);
        assert!(work.structural_transitions <= 4096);
        if ready {
            break preparation.take_ready_for(document).unwrap().unwrap();
        }
    };
    let source = display.canonical_raw_cell().clone();
    let observer = display.resource_observer();
    request.cancel();
    drop((display, preparation, publication, request));
    let released = observer.snapshot();
    assert!(released.source_payload_released);
    assert!(!released.origin_file_open);
    assert_eq!(released.origin_buffer_bytes, 0);
    assert!(!released.origin_cleanup_failed);
    assert!(!released.origin_reader_failed);
    source
}

#[cfg(feature = "paged-line-compose")]
#[test]
/// 正式Display原票激活后，CRLF合并、管道转义、代理对和首尾trim必须同时保住原选择方向。
fn original_display_commit_maps_directed_selection_in_the_actual_sanitize_transaction() {
    for (raw, expected, anchor, head, mapped_anchor, mapped_head, word_start, mapped_start) in [
        ("  中|😀\r\n尾 \u{2003}", "中\\|😀 尾", 11, 4, 7, 3, 7, 6),
        ("\u{2003}\t\r\n  ", "", 1, 6, 0, 0, 6, 0),
        (r"x\\|中\|z", r"x\\\|中\|z", 3, 7, 3, 8, 4, 5),
        ("a\r\n\nb", "a b", 2, 4, 2, 2, 3, 2),
        ("😀|x", "😀\\|x", 2, 3, 2, 4, 2, 2),
        ("  x|", "x\\|", 2, 4, 0, 3, 0, 0),
    ] {
        let (_directory, mut doc) = document(SOURCE);
        let mapping = doc.original_backing_observer().unwrap();
        let source = displayed_cell(&doc, SOURCE.find("| a |").unwrap(), 0);
        let plan = doc.plan_verified_table_cell_draft(source).unwrap();
        let token = doc.begin_prepared_cell_draft(plan).unwrap();
        let before = doc.cell_draft_facts(&token).unwrap();
        let resources = doc.cell_draft_resource_observer(&token).unwrap();
        edit(
            &mut doc,
            &token,
            0..before.utf16_len,
            raw,
            MarkdownTextSelection {
                anchor_utf16: anchor,
                head_utf16: head,
            },
            None,
        )
        .unwrap();
        let first = doc.capture_cell_draft(&token).unwrap();
        let second = doc.capture_cell_draft(&token).unwrap();
        let prepared = doc
            .prepare_cell_draft_commit(
                doc.capture_cell_draft_edit(&token).unwrap(),
                Some(MarkdownCellDraftWordAnchor {
                    range: MarkdownTextSelection {
                        anchor_utf16: word_start,
                        head_utf16: raw.encode_utf16().count(),
                    },
                    origin_utf16: word_start,
                }),
            )
            .unwrap();
        assert_eq!(canonical(&doc), SOURCE); // 原两个显示读取仍活，准备不能提前发布或移动正文选择。
        assert_eq!(doc.selection(), MarkdownTextSelection::default());
        drop((first, second));
        let committed = doc
            .commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap();
        assert!(committed.edit.is_some());
        assert_eq!(
            committed.receipt.selection,
            MarkdownTextSelection {
                anchor_utf16: before.canonical_range.start + mapped_anchor,
                head_utf16: before.canonical_range.start + mapped_head,
            },
            "raw={raw:?}"
        );
        assert_eq!(committed.receipt.next_cell, None);
        // 原首组两端独立于当前选区；CRLF、管道转义和尾trim在同一趟映射中保留组范围。
        assert_eq!(
            committed.receipt.word_anchor,
            Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: before.canonical_range.start + mapped_start,
                    head_utf16: before.canonical_range.start + expected.encode_utf16().count()
                },
                origin_utf16: before.canonical_range.start + mapped_start
            }),
            "raw={raw:?}"
        );
        let after = SOURCE.replacen("| a |", &format!("| {expected} |"), 1);
        assert_eq!(canonical(&doc), after);
        // 端点收据不暗改旧成功选择合同；一次canonical事务仍将caret放在实际替换正文后。
        assert_eq!(
            doc.selection(),
            MarkdownTextSelection::caret(
                before.canonical_range.start + expected.encode_utf16().count()
            )
        );
        assert!(resources.is_released());
        assert_eq!(std::fs::read(doc.path()).unwrap(), SOURCE.as_bytes());
        assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_some());
        assert_eq!(canonical(&doc), SOURCE);
        assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_none());
        assert!(doc.redo_text_edit_observed(NoopObserver).unwrap().is_some());
        assert_eq!(canonical(&doc), after);
        drop(doc);
        assert!(mapping.unmap_succeeded() && !mapping.failed());
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
/// 未改内容仍交付真实trim端点却不动Doc；缺格补分隔符后的正文位置不能误当原插入锚点。
fn original_display_commit_receipts_distinguish_unchanged_and_implicit_cell_positions() {
    let (_directory, mut doc) = document(SOURCE);
    let mapping = doc.original_backing_observer().unwrap();
    doc.set_selection(MarkdownTextSelection::caret(1)).unwrap();
    let source = displayed_cell(&doc, SOURCE.find("| a |").unwrap(), 0);
    let plan = doc.plan_verified_table_cell_draft(source).unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let resources = doc.cell_draft_resource_observer(&token).unwrap();
    let profile = doc.storage_profile();
    edit(
        &mut doc,
        &token,
        0..before.utf16_len,
        "  a  ",
        MarkdownTextSelection {
            anchor_utf16: 5,
            head_utf16: 2,
        },
        None,
    )
    .unwrap();
    let committed = doc
        .commit_prepared_cell_draft_observed(
            doc.prepare_cell_draft_commit(
                doc.capture_cell_draft_edit(&token).unwrap(),
                Some(MarkdownCellDraftWordAnchor {
                    range: MarkdownTextSelection::caret(2),
                    origin_utf16: 2,
                }),
            )
            .unwrap(),
            NoopObserver,
        )
        .unwrap();
    assert!(committed.edit.is_none());
    assert_eq!(
        committed.receipt.word_anchor,
        Some(MarkdownCellDraftWordAnchor {
            range: MarkdownTextSelection::caret(before.canonical_range.start),
            origin_utf16: before.canonical_range.start
        })
    );
    assert_eq!(
        committed.receipt.selection,
        MarkdownTextSelection {
            anchor_utf16: before.canonical_range.start + 1,
            head_utf16: before.canonical_range.start,
        }
    );
    assert_eq!(committed.receipt.next_cell, None);
    assert_eq!(doc.selection(), MarkdownTextSelection::caret(1));
    assert_eq!(doc.generation(), 0);
    assert_eq!(doc.storage_profile(), profile);
    assert_eq!(canonical(&doc), SOURCE);
    assert!(!doc.can_undo());
    assert!(resources.is_released());
    drop(doc);
    assert!(mapping.unmap_succeeded() && !mapping.failed());

    const IMPLICIT: &str = "| H | E | M | N |\r\n| --- | --- | --- | --- |\r\n| a ||\r\n";
    const EXPECTED: &str = "| H | E | M | N |\r\n| --- | --- | --- | --- |\r\n| a || | 中\\|😀\r\n";
    let (_directory, mut doc) = document(IMPLICIT);
    let mapping = doc.original_backing_observer().unwrap();
    let source = displayed_cell(&doc, IMPLICIT.find("| a ||").unwrap(), 3);
    let plan = doc.plan_verified_table_cell_draft(source).unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    let resources = doc.cell_draft_resource_observer(&token).unwrap();
    edit(
        &mut doc,
        &token,
        0..0,
        " \t中|😀  ",
        MarkdownTextSelection {
            anchor_utf16: 8,
            head_utf16: 3,
        },
        None,
    )
    .unwrap();
    let prepared = doc
        .prepare_cell_draft_commit(doc.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    let committed = doc
        .commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap();
    let text_start = EXPECTED[..EXPECTED.find('中').unwrap()]
        .encode_utf16()
        .count();
    assert!(committed.edit.is_some());
    assert_eq!(
        committed.receipt.selection,
        MarkdownTextSelection {
            anchor_utf16: text_start + 5,
            head_utf16: text_start + 1,
        }
    );
    assert_eq!(canonical(&doc), EXPECTED);
    assert_eq!(std::fs::read(doc.path()).unwrap(), IMPLICIT.as_bytes());
    assert!(resources.is_released());
    assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_some());
    assert_eq!(canonical(&doc), IMPLICIT);
    assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_none());
    drop(doc);
    assert!(mapping.unmap_succeeded() && !mapping.failed());
}

#[cfg(feature = "paged-line-compose")]
#[test]
/// 原首词组两端都属于当前草稿serial和真实标量；任一端点无效都不能发布或取消分支。
fn original_display_word_group_rejects_surrogate_and_stale_input_then_commits_once() {
    let (_directory, mut doc) = document(SOURCE);
    let mapping = doc.original_backing_observer().unwrap();
    let source = displayed_cell(&doc, SOURCE.find("| a |").unwrap(), 0);
    let plan = doc.plan_verified_table_cell_draft(source).unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    let before = doc.cell_draft_facts(&token).unwrap();
    let resources = doc.cell_draft_resource_observer(&token).unwrap();
    edit(
        &mut doc,
        &token,
        0..before.utf16_len,
        " \t😀|中 \u{2003}",
        MarkdownTextSelection::caret(8),
        None,
    )
    .unwrap();
    let facts = doc.cell_draft_facts(&token).unwrap();
    for (group, origin, expected) in [
        (
            MarkdownTextSelection {
                anchor_utf16: 3,
                head_utf16: 4,
            },
            4,
            MarkdownDocumentBufferError::NotUnicodeBoundary,
        ),
        (
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 3,
            },
            4,
            MarkdownDocumentBufferError::NotUnicodeBoundary,
        ),
        (
            MarkdownTextSelection {
                anchor_utf16: 9,
                head_utf16: 4,
            },
            4,
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
        (
            MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 9,
            },
            4,
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
        (
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 6,
            },
            3,
            MarkdownDocumentBufferError::NotUnicodeBoundary,
        ),
        (
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 6,
            },
            9,
            MarkdownDocumentBufferError::RangeOutsideDocument,
        ),
    ] {
        let prepared = doc.prepare_cell_draft_commit(
            doc.capture_cell_draft_edit(&token).unwrap(),
            Some(MarkdownCellDraftWordAnchor {
                range: group,
                origin_utf16: origin,
            }),
        );
        assert!(matches!(
            prepared,
            Err(MarkdownCellDraftError::Document(error)) if error == expected
        ));
        // 两类真实输入错误都保留同一可编辑草稿；不能把无效首组端点钳到另一字素冒充原Word动作。
        assert_eq!(doc.cell_draft_facts(&token).unwrap(), facts);
        assert_eq!(canonical(&doc), SOURCE);
        assert!(!doc.can_undo());
    }
    let stale = doc.capture_cell_draft_edit(&token).unwrap();
    doc.set_cell_draft_selection(
        doc.capture_cell_draft_edit(&token).unwrap(),
        MarkdownTextSelection::caret(2),
    )
    .unwrap();
    assert!(matches!(
        doc.prepare_cell_draft_commit(
            stale,
            Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: 4,
                    head_utf16: 5
                },
                origin_utf16: 4
            })
        ),
        Err(MarkdownCellDraftError::Stale)
    )); // 相同正文和合法scalar也不能给旧输入serial补签首词组。
    let prepared = doc
        .prepare_cell_draft_commit(
            doc.capture_cell_draft_edit(&token).unwrap(),
            Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: 4,
                    head_utf16: 5,
                },
                origin_utf16: 4,
            }),
        )
        .unwrap();
    let committed = doc
        .commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap();
    assert!(committed.edit.is_some());
    assert_eq!(
        committed.receipt.word_anchor,
        Some(MarkdownCellDraftWordAnchor {
            range: MarkdownTextSelection {
                anchor_utf16: before.canonical_range.start + 2,
                head_utf16: before.canonical_range.start + 4
            },
            origin_utf16: before.canonical_range.start + 2
        })
    );
    assert_eq!(
        committed.receipt.selection,
        MarkdownTextSelection::caret(before.canonical_range.start)
    );
    assert_eq!(canonical(&doc), SOURCE.replacen("| a |", "| 😀\\|中 |", 1));
    assert_eq!(std::fs::read(doc.path()).unwrap(), SOURCE.as_bytes());
    assert!(resources.is_released());
    assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_some());
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_none());
    drop(doc);
    assert!(mapping.unmap_succeeded() && !mapping.failed());
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn original_display_cell_ticket_edits_explicit_and_missing_cells_with_one_canonical_undo() {
    let header = "| H | E | M | N |\r\n| --- | --- | --- | --- |\r\n";
    let text = format!("{header}| a ||\r\n");
    for (column, row_after) in [
        (1, "| a |中\\|文 尾|\r\n"),
        (2, "| a ||中\\|文 尾\r\n"),
        (3, "| a || | 中\\|文 尾\r\n"),
    ] {
        // 相同零长显示分别覆盖显式空格、首个缺格和中间尚缺一列的目标；预期来自固定原文件。
        let (_directory, mut document) = document(&text);
        let original = document.original_backing_observer().unwrap();
        let source = displayed_cell(&document, header.len(), column);
        // 相同文件、长度和行列不构成同 Doc 权限；原封闭格票不能激活另一个映射的草稿。
        let foreign = MarkdownDocumentBuffer::open(document.path(), 32 * 1024 * 1024).unwrap();
        assert!(matches!(
            foreign.plan_verified_table_cell_draft(source.clone()),
            Err(MarkdownTableRequestError::SourceSuperseded)
        ));
        drop(foreign);
        let plan = document.plan_verified_table_cell_draft(source).unwrap();
        let token = document.begin_prepared_cell_draft(plan).unwrap();
        let draft_resources = document.cell_draft_resource_observer(&token).unwrap();
        let edit = document.capture_cell_draft_edit(&token).unwrap();
        let input = "中|文\n尾";
        document
            .replace_cell_draft(
                edit,
                0..0,
                input,
                MarkdownTextSelection::caret(input.encode_utf16().count()),
                None,
            )
            .unwrap();
        let prepared = document
            .prepare_cell_draft_commit(document.capture_cell_draft_edit(&token).unwrap(), None)
            .unwrap();
        document
            .commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .edit
            .unwrap();
        let mut actual = Vec::new(); // 固定短表可输出独立 oracle；巨格在下一项只用固定窗口比较。
        document.write_to(&mut actual).unwrap();
        assert_eq!(actual, format!("{header}{row_after}").as_bytes());
        assert!(draft_resources.is_released());
        assert_eq!(std::fs::read(document.path()).unwrap(), text.as_bytes());
        // canonical 只需一次 Undo 恢复精确原字节；第二次没有事务，不能先提交再补列。
        assert!(
            document
                .undo_text_edit_observed(NoopObserver)
                .unwrap()
                .is_some()
        );
        actual.clear();
        document.write_to(&mut actual).unwrap();
        assert_eq!(actual, text.as_bytes());
        assert!(
            document
                .undo_text_edit_observed(NoopObserver)
                .unwrap()
                .is_none()
        );
        drop(document);
        assert!(original.unmap_succeeded());
        assert!(!original.failed());
    }
}

#[cfg(feature = "paged-line-compose")]
#[test]
fn original_two_mib_display_ticket_commits_and_undoes_without_a_whole_cell_string() {
    use std::io::{Read, Seek, SeekFrom, Write};

    const HEADER: &str = "| H | C |\r\n| --- | --- |\r\n| ";
    const TAIL: &str = "中😀 | other |\r\n";
    const BODY_BYTES: usize = 2 * 1024 * 1024;
    let ascii_bytes = BODY_BYTES - "中😀".len();
    let page = [b'a'; 4096];
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(HEADER.as_bytes()).unwrap();
    for _ in 0..ascii_bytes / page.len() {
        file.write_all(&page).unwrap();
    }
    file.write_all(&page[..ascii_bytes % page.len()]).unwrap();
    file.write_all(TAIL.as_bytes()).unwrap();
    file.flush().unwrap();
    // 文件按页生成；测试也不构造 2MiB payload/String 或通过有限完整窗口激活巨格。
    let mut document = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    let original = document.original_backing_observer().unwrap();
    let source = displayed_cell(&document, HEADER.len() - 2, 0);
    let plan = document
        .plan_verified_table_cell_draft(source.clone())
        .unwrap();
    let token = document.begin_prepared_cell_draft(plan).unwrap();
    let draft_resources = document.cell_draft_resource_observer(&token).unwrap();
    let facts = document.cell_draft_facts(&token).unwrap();
    assert_eq!(facts.byte_len, BODY_BYTES);
    let edit = document.capture_cell_draft_edit(&token).unwrap();
    document
        .replace_cell_draft(
            edit,
            facts.utf16_len..facts.utf16_len,
            "尾",
            MarkdownTextSelection::caret(facts.utf16_len + 1),
            None,
        )
        .unwrap();
    let prepared = document
        .prepare_cell_draft_commit(document.capture_cell_draft_edit(&token).unwrap(), None)
        .unwrap();
    document
        .commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap()
        .edit
        .unwrap();
    assert!(draft_resources.is_released());

    // 每个实际 canonical 字符都与独立的生成规则比较，工作数组固定 4KiB；不只核首尾或长度。
    let assert_canonical = |document: &MarkdownDocumentBuffer, tail: &str| {
        let mut units = [0; 2048];
        document
            .copy_utf16_code_units(0, &mut units[..HEADER.len()])
            .unwrap();
        assert!(
            units[..HEADER.len()]
                .iter()
                .copied()
                .eq(HEADER.encode_utf16())
        );
        let mut at = HEADER.len();
        while at < HEADER.len() + ascii_bytes {
            let count = units.len().min(HEADER.len() + ascii_bytes - at);
            document
                .copy_utf16_code_units(at, &mut units[..count])
                .unwrap();
            assert!(units[..count].iter().all(|unit| *unit == u16::from(b'a')));
            at += count;
        }
        let tail_len = tail.encode_utf16().count();
        document
            .copy_utf16_code_units(at, &mut units[..tail_len])
            .unwrap();
        assert!(units[..tail_len].iter().copied().eq(tail.encode_utf16()));
        assert_eq!(document.utf16_len(), at + tail_len);
    };
    assert_canonical(&document, "中😀尾 | other |\r\n");
    assert!(matches!(
        document.plan_verified_table_cell_draft(source.clone()),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert!(
        document
            .undo_text_edit_observed(NoopObserver)
            .unwrap()
            .is_some()
    );
    assert_canonical(&document, TAIL);
    // Undo 恢复原内容也没有恢复旧手势/原票的权限，下一次必须消费新的正式来源发布。
    assert!(matches!(
        document.plan_verified_table_cell_draft(source.clone()),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert!(
        document
            .undo_text_edit_observed(NoopObserver)
            .unwrap()
            .is_none()
    );
    assert!(
        document
            .redo_text_edit_observed(NoopObserver)
            .unwrap()
            .is_some()
    );
    assert_canonical(&document, "中😀尾 | other |\r\n");
    assert!(
        document
            .redo_text_edit_observed(NoopObserver)
            .unwrap()
            .is_none()
    );

    // 编辑、Undo/Redo 均未保存；原磁盘仍按每个真实页比对，不能改写原文件冒充低内存编辑。
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut disk = [0; 4096];
    file.read_exact(&mut disk[..HEADER.len()]).unwrap();
    assert_eq!(&disk[..HEADER.len()], HEADER.as_bytes());
    let mut remaining = ascii_bytes;
    while remaining != 0 {
        let count = disk.len().min(remaining);
        file.read_exact(&mut disk[..count]).unwrap();
        assert_eq!(&disk[..count], &page[..count]);
        remaining -= count;
    }
    file.read_exact(&mut disk[..TAIL.len()]).unwrap();
    assert_eq!(&disk[..TAIL.len()], TAIL.as_bytes());
    assert_eq!(file.read(&mut disk[..1]).unwrap(), 0);
    let backings: Vec<_> = document.backing_observers().collect();
    drop(document);
    // 保留的原 raw 票和 observer 只能认原身份，不能延长实际 mmap 或提交文件的寿命。
    assert!(original.unmap_succeeded());
    assert!(!original.failed());
    for backing in backings {
        assert!(backing.unmap_succeeded());
        assert_eq!(backing.mapped_bytes(), 0);
        assert!(!backing.failed());
    }
    drop(source);
}
