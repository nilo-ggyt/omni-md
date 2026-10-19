//! 真实 source wrapper 续批回归；完整小夹具只用于独立字面来源和一次 Undo 的测试 oracle。

use super::*;
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource;
use std::sync::Arc;

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("同源单行.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}

fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
    MarkdownViewportSourceAuthority {
        context_revision: 71,
        selection: document.selection(),
        cancelled: false,
    }
}

fn units(source: &str, byte: usize) -> usize {
    source[..byte].encode_utf16().count()
}

fn finish(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> Result<(), MarkdownTableRequestError> {
    for _ in 0..document
        .byte_len()
        .saturating_mul(100)
        .saturating_add(10000)
    {
        // 只有测试驱动循环；每次产品 quantum 的工作上限仍独立断言，不用循环替生产异步完成。
        match request.advance(document, authority(document), 4096)? {
            MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work } => {
                assert!(
                    work.source_bytes <= 4096
                        && work.structural_transitions <= 4096
                        && work.metadata_bytes <= 4096
                );
            }
        }
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("有限 source-required 夹具未结束");
}

fn take(
    request: &mut MarkdownViewportSourceRequest,
    document: &MarkdownDocumentBuffer,
) -> VerifiedMarkdownViewportSources {
    request
        .take_sources_for(document, authority(document))
        .unwrap()
        .unwrap()
}

fn row_result(
    sources: &VerifiedMarkdownViewportSources,
    document: &MarkdownDocumentBuffer,
    expected: usize,
) -> usize {
    match sources
        .required_row_result_for(document, authority(document))
        .unwrap()
        .unwrap()
    {
        MarkdownTableViewportRequiredRowResult::Row {
            window_index,
            row_ordinal,
        } => {
            assert_eq!(row_ordinal, expected);
            window_index
        }
        MarkdownTableViewportRequiredRowResult::TableEnd { .. } => {
            panic!("真实成员不能被 End 冒充")
        }
    }
}

#[test]
fn real_previous_row_uses_both_original_publications_and_same_global() {
    let source =
        "前段\n\n| H | C |\n| - | - |\n| 中😀 | 一 |\n| second | 二 |\n| third | 三 |\n\n尾段\n";
    let (_directory, doc) = document(source);
    let third = units(source, source.find("| third").unwrap());
    let viewport = third..third + 4;
    let mut request = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = take(&mut request, &doc);
    assert!(
        previous
            .required_row_result_for(&doc, authority(&doc))
            .unwrap()
            .is_none()
    );
    let (global, next_byte, _) = request.batch.source_probe_state();
    let work = request.total_work();
    let root_owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let query = previous
        .required_row_query_for(&doc, authority(&doc), 0, 3, 1)
        .unwrap();
    request
        .continue_forward_with_required_row(&doc, &previous, viewport, authority(&doc), query)
        .unwrap();
    assert!(
        request
            .sources_for(&doc, authority(&doc))
            .unwrap()
            .is_none(),
        "启动仅为 Pending，不能把旧 SourcesAvailable 当目标完成"
    );
    finish(&mut request, &doc).unwrap();
    let ready = take(&mut request, &doc);
    let index = row_result(&ready, &doc, 1);
    let table = ready.table_sources_for(&doc, authority(&doc)).unwrap();
    let window = table
        .window_at_for(&doc, index)
        .unwrap()
        .unwrap()
        .presentation_for(&doc)
        .unwrap();
    let start = units(source, source.find("中😀").unwrap());
    assert_eq!(
        window.cell(1, 0).unwrap().source,
        MarkdownTableWindowCellSource::Present(start..start + 3)
    );
    assert!(window.row(1).is_some());
    assert_eq!(request.batch.source_probe_state().0, global);
    assert_eq!(
        request.batch.source_probe_state().1,
        next_byte,
        "单表 checkpoint 回放不能推进主 Global 越过普通来源"
    );
    assert!(request.total_work().source_bytes > work.source_bytes);
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), root_owners);
    assert!(
        previous
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .window_at_for(&doc, 0)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .row(3)
            .is_some()
    );
}

#[test]
fn required_continuation_keeps_unfinished_ordinary_cursor_and_real_forward_floor() {
    let mut source = String::from("| H |\n| - |\n| one |\n| two |\n\n");
    let mut starts = Vec::new();
    for index in 0..120 {
        starts.push(source.len());
        source.push_str(&format!("普通{index}中😀\r\n"));
    }
    let (_directory, doc) = document(&source);
    let viewport = 0..units(&source, starts[5]);
    let mut request = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = take(&mut request, &doc);
    let cursor = match request.job.as_ref().unwrap() {
        OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
        OrdinaryJob::Leaf(_) => panic!("夹具尾部必须是真实未完 paragraph"),
    };
    let global = request.batch.source_probe_state().0;
    assert_eq!(request.origin_forward_floor(), viewport.end);
    assert!(request.batch.forward_resume_floor() > request.origin_forward_floor());
    let next = 0..units(&source, starts[10]);
    let query = previous
        .required_row_query_for(&doc, authority(&doc), 0, 1, 2)
        .unwrap();
    request
        .continue_forward_with_required_row(&doc, &previous, next.clone(), authority(&doc), query)
        .unwrap();
    assert_eq!(
        match request.job.as_ref().unwrap() {
            OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
            _ => 0,
        },
        cursor
    );
    finish(&mut request, &doc).unwrap();
    let ready = take(&mut request, &doc);
    row_result(&ready, &doc, 2);
    let lines = ready
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 10);
    for (line, start) in lines.iter().zip(&starts[..10]) {
        // 来源期待值来自夹具的真实物理行，不能用分类水位或 RequiredRow 输出补造普通记录。
        assert_eq!(line.physical_utf16_range().start, units(&source, *start));
    }
    assert_eq!(request.origin_forward_floor(), next.end);
    assert_eq!(request.batch.source_probe_state().0, global);
    assert_eq!(
        match request.job.as_ref().unwrap() {
            OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
            _ => 0,
        },
        cursor
    );
    assert_eq!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        5
    );
}

#[test]
fn actual_end_from_same_sources_authorizes_single_append_and_one_undo() {
    let source = "> - | H | C |\r\n>   | - | - |\r\n>   | 中 | 原 |";
    let (_directory, mut doc) = document(source);
    let at = units(source, source.find(">   | 中").unwrap());
    let viewport = at..at + 4;
    let mut request = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = take(&mut request, &doc);
    let query = previous
        .required_row_query_for(&doc, authority(&doc), 0, 1, 999)
        .unwrap();
    request
        .continue_forward_with_required_row(&doc, &previous, viewport, authority(&doc), query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let ready = take(&mut request, &doc);
    let index = match ready
        .required_row_result_for(&doc, authority(&doc))
        .unwrap()
        .unwrap()
    {
        MarkdownTableViewportRequiredRowResult::TableEnd {
            end,
            terminal_window_index,
        } => {
            assert!(end.ended_at_source_eof() && !end.ended_with_eol());
            assert_eq!(end.last_body_row_ordinal(), Some(1));
            terminal_window_index
        }
        MarkdownTableViewportRequiredRowResult::Row { .. } => {
            panic!("不存在的成员必须由真实 End 结束")
        }
    };
    let plan = doc
        .plan_verified_table_viewport_row_append(
            ready.table_sources_for(&doc, authority(&doc)).unwrap(),
            index,
            1,
            1,
            "草稿😀",
        )
        .unwrap();
    let (range, replacement) = plan.replacement_for(&doc).unwrap();
    let (range, replacement) = (range.clone(), replacement.to_owned());
    let revision = doc.current_revision().unwrap();
    doc.replace_utf16_range(range, &replacement, MarkdownTextSelection::caret(0))
        .unwrap();
    assert_eq!(doc.generation(), 1);
    assert_eq!(
        doc.copy_table_source_range(0..doc.byte_len(), 65536)
            .unwrap(),
        "> - | H | C |\r\n>   | - | - |\r\n>   | 中 | 草稿😀 |\r\n>   | | |"
    );
    assert!(
        ready
            .required_row_result_for(&doc, authority(&doc))
            .is_err()
    );
    doc.undo().unwrap();
    assert_eq!(doc.current_revision().unwrap(), revision);
    assert_eq!(
        doc.copy_table_source_range(0..doc.byte_len(), 65536)
            .unwrap(),
        source
    );
    assert!(
        ready
            .required_row_result_for(&doc, authority(&doc))
            .is_err(),
        "Undo 同字节仍是新代次，旧结果不能复活"
    );
}

#[test]
fn wrong_source_and_inner_table_publications_or_reverse_viewport_preserve_retry() {
    let source = "| H |\n| - |\n| one |\n| two |\n| three |\n";
    let (_directory, doc) = document(source);
    let at = source.find("| three").unwrap();
    let viewport = at..at + 4;
    let mut request = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    let mut other = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    finish(&mut other, &doc).unwrap();
    let previous = take(&mut request, &doc);
    let foreign = take(&mut other, &doc);
    let query = foreign
        .required_row_query_for(&doc, authority(&doc), 0, 3, 1)
        .unwrap();
    assert_eq!(
        request.continue_forward_with_required_row(
            &doc,
            &previous,
            viewport.clone(),
            authority(&doc),
            query
        ),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    // 测试内故意把外层 Weak 换成正确票；内层 opaque TablePublication 仍必须独立拒绝。
    let mut query = foreign
        .required_row_query_for(&doc, authority(&doc), 0, 3, 1)
        .unwrap();
    query.publication = Rc::downgrade(&previous.publication);
    assert_eq!(
        request.continue_forward_with_required_row(
            &doc,
            &previous,
            viewport.clone(),
            authority(&doc),
            query
        ),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    );
    let query = previous
        .required_row_query_for(&doc, authority(&doc), 0, 3, 1)
        .unwrap();
    assert_eq!(
        request.continue_forward_with_required_row(
            &doc,
            &previous,
            0..viewport.end,
            authority(&doc),
            query
        ),
        Err(MarkdownTableRequestError::CoverageRequiresCheckpoint)
    );
    let query = previous
        .required_row_query_for(&doc, authority(&doc), 0, 3, 1)
        .unwrap();
    request
        .continue_forward_with_required_row(&doc, &previous, viewport, authority(&doc), query)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    row_result(&take(&mut request, &doc), &doc, 1);
}

#[test]
fn actual_reverse_authority_cancel_and_stale_source_are_sticky_and_release_work() {
    let source = "| H |\n| - |\n| one |\n| two |\n";
    for failure in 0..3 {
        let (_directory, mut doc) = document(source);
        doc.set_selection(MarkdownTextSelection {
            anchor_utf16: 7,
            head_utf16: 2,
        })
        .unwrap();
        let viewport = 0..doc.utf16_len();
        let mut request = doc
            .viewport_source_request(viewport.clone(), 71, doc.selection())
            .unwrap();
        finish(&mut request, &doc).unwrap();
        let previous = take(&mut request, &doc);
        let query = previous
            .required_row_query_for(&doc, authority(&doc), 0, 1, 2)
            .unwrap();
        let mut observed = authority(&doc);
        let expected = match failure {
            0 => {
                observed.selection = MarkdownTextSelection {
                    anchor_utf16: 2,
                    head_utf16: 7,
                };
                MarkdownTableRequestError::PublicationSuperseded
            }
            1 => {
                observed.cancelled = true;
                MarkdownTableRequestError::Cancelled
            }
            _ => {
                doc.replace_utf16_range(0..0, "x", MarkdownTextSelection::caret(0))
                    .unwrap();
                observed = authority(&doc);
                MarkdownTableRequestError::SourceSuperseded
            }
        };
        assert_eq!(
            request.continue_forward_with_required_row(&doc, &previous, viewport, observed, query),
            Err(expected)
        );
        assert_eq!(request.advance(&doc, authority(&doc), 4096), Err(expected));
        assert!(
            request.records.is_empty() && request.job.is_none() && request.coordinates.is_none()
        );
    }
}

#[test]
fn previous_ordinary_table_and_required_inventory_share_the_same_cell_limit() {
    let mut source = "普通行\n".repeat(20);
    source.push('\n');
    source.push_str(&format!("|{}\n|{}\n", " H |".repeat(64), " - |".repeat(64)));
    for _ in 0..14 {
        source.push_str(&format!("|{}\n", " x |".repeat(64)));
    }
    let old_end = source.len();
    let target_start = old_end;
    source.push_str(&format!("|{}\n", " y |".repeat(64)));
    let (_directory, doc) = document(&source);
    let viewport = 0..units(&source, old_end);
    let mut request = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = take(&mut request, &doc);
    assert_eq!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        20
    );
    assert_eq!(
        previous
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .budget()
            .cells,
        960
    );
    let before = request.total_work();
    let query = previous
        .required_row_query_for(&doc, authority(&doc), 0, 14, 15)
        .unwrap();
    request
        .continue_forward_with_required_row(&doc, &previous, viewport, authority(&doc), query)
        .unwrap();
    assert!(request.total_work().structural_transitions > before.structural_transitions);
    let expected = MarkdownTableRequestError::Window(MarkdownTableWindowError::CellLimit);
    assert_eq!(finish(&mut request, &doc), Err(expected));
    assert_eq!(request.advance(&doc, authority(&doc), 4096), Err(expected));
    assert_eq!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        20
    );
    assert!(
        previous
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .windows_for(&doc)
            .unwrap()
            .next()
            .is_some()
    );
    // 目标行单独合法；失败来自同活库存联合准入，而不是用超大或损坏单行强造拒绝。
    let target = units(&source, target_start)..units(&source, target_start) + 4;
    let mut independent = doc
        .viewport_source_request(target, 71, doc.selection())
        .unwrap();
    finish(&mut independent, &doc).unwrap();
}

#[test]
fn repeated_required_replay_keeps_original_total_fuel() {
    let source = "| H |\n| - |\n| one |\n| two |\n| three |\n| four |\n";
    let (_directory, doc) = document(source);
    let at = source.find("| four").unwrap();
    let viewport = at..at + 4;
    let mut request = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut previous = take(&mut request, &doc);
    let maximum = request.batch.source_limits().unwrap().1 / CONTROL_COST + 1;
    let (mut index, mut origin, mut target) = (0, 4, 1);
    let mut exhausted = false;
    for _ in 0..maximum {
        let query = previous
            .required_row_query_for(&doc, authority(&doc), index, origin, target)
            .unwrap();
        let result = request
            .continue_forward_with_required_row(
                &doc,
                &previous,
                viewport.clone(),
                authority(&doc),
                query,
            )
            .and_then(|()| finish(&mut request, &doc));
        match result {
            Ok(()) => {
                let ready = take(&mut request, &doc);
                index = row_result(&ready, &doc, target);
                previous = ready;
                origin = target;
                target = target % 3 + 1; // 三个真实成员轮换，避免只读当前已存在行而不发生回放。
            }
            Err(error) => {
                assert_eq!(error, MarkdownTableRequestError::WorkLimit);
                exhausted = true;
                break;
            }
        }
    }
    assert!(exhausted, "同 owner RequiredRow 不能为每次续批重授总 fuel");
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096),
        Err(MarkdownTableRequestError::WorkLimit)
    );
    assert!(
        previous
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .window_at_for(&doc, index)
            .unwrap()
            .unwrap()
            .presentation_for(&doc)
            .unwrap()
            .row(origin)
            .is_some()
    );
    assert_eq!(doc.generation(), 0);
}

#[test]
fn sources_query_and_required_request_keep_only_weak_document_ownership() {
    let source = "| H |\n| - |\n| one |\n| two |\n\n普通\n";
    let (_directory, doc) = document(source);
    let viewport = 0..doc.utf16_len();
    let root = Arc::downgrade(doc.root.as_ref().unwrap());
    let mut request = doc
        .viewport_source_request(viewport.clone(), 71, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = take(&mut request, &doc);
    let query = previous
        .required_row_query_for(&doc, authority(&doc), 0, 1, 999)
        .unwrap();
    request
        .continue_forward_with_required_row(&doc, &previous, viewport, authority(&doc), query)
        .unwrap();
    drop(doc);
    assert_eq!(
        root.strong_count(),
        0,
        "query、旧 publication 和未完成回放都不能保活 piece root/mmap"
    );
    request.cancel();
    assert!(request.records.is_empty() && request.job.is_none() && request.coordinates.is_none());
    assert!(request.ready.is_none() && request.last_publication.is_none());
}
