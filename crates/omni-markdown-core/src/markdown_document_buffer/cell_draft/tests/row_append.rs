//! 原结构来源、真实 terminal 与活动草稿的一次提交；只使用公开生产流程和实际映射释放收据。

use super::*;
use crate::markdown_viewport_presentation::table_window::navigation::MarkdownTableWindowNavigation;

/// 夹具拥有唯一实际 Source 请求和当前发布；格票来自它的原 CellInline，不另起窗口或 Parser。
struct CellOrigin {
    request: MarkdownViewportSourceRequest,
    sources: VerifiedMarkdownViewportSources,
    row: VerifiedMarkdownTableRowSource,
    cell: VerifiedMarkdownTableCellSource,
    authority: MarkdownViewportSourceAuthority,
}

fn finish(
    request: &mut MarkdownViewportSourceRequest,
    doc: &MarkdownDocumentBuffer,
    authority: MarkdownViewportSourceAuthority,
) -> VerifiedMarkdownViewportSources {
    loop {
        let work = match request.advance(doc, authority, 4096).unwrap() {
            MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work } => work,
        };
        assert!(work.source_bytes <= 4096);
        assert!(work.copied_source_bytes <= 4096);
        assert!(work.structural_transitions <= 4096);
        if let Some(sources) = request.take_sources_for(doc, authority).unwrap() {
            assert!(
                sources
                    .table_sources_for(doc, authority)
                    .unwrap()
                    .windows_for(doc)
                    .unwrap()
                    .next()
                    .is_none()
            );
            return sources;
        }
    }
}

impl CellOrigin {
    fn new(doc: &MarkdownDocumentBuffer, row: usize, column: usize) -> Self {
        let authority = MarkdownViewportSourceAuthority {
            context_revision: 73,
            selection: doc.selection(),
            cancelled: false,
        };
        let mut request = doc
            .viewport_leaf_source_request(0..1, 73, doc.selection())
            .unwrap();
        let mut sources = finish(&mut request, doc, authority);
        let mut actual = sources
            .table_row_at_for(doc, authority, 0)
            .unwrap()
            .unwrap();
        for _ in 0..row {
            // 测试已知目标只决定走几次真实相邻查询，整数不会直接成为格来源或 checkpoint。
            let query = sources
                .required_row_query_for(
                    doc,
                    authority,
                    &actual,
                    MarkdownTableWindowNavigation::Next,
                )
                .unwrap()
                .unwrap();
            request
                .continue_forward_with_required_row(doc, &sources, 0..1, authority, query)
                .unwrap();
            sources = finish(&mut request, doc, authority);
            let MarkdownViewportSourceRequiredRowResult::Row { row } = sources
                .required_row_result_for(doc, authority)
                .unwrap()
                .unwrap()
            else {
                panic!("夹具请求的成员必须真实存在")
            };
            actual = row;
        }
        request
            .prepare_cell_inline(doc, actual.clone(), authority, column)
            .unwrap();
        let cell = loop {
            let step = request.advance_cell_inline(doc, authority, 4096).unwrap();
            let (ready, work) = match step {
                MarkdownTableCellInlineStep::Progress { work } => (false, work),
                MarkdownTableCellInlineStep::Ready { work } => (true, work),
            };
            assert!(work.source_bytes <= 4096);
            assert!(work.copied_source_bytes <= 4096);
            assert!(work.structural_transitions <= 4096);
            if ready {
                let inline = request
                    .take_cell_inline_ready_for(doc, authority)
                    .unwrap()
                    .unwrap();
                break inline.cell_source_for(doc, authority).unwrap().clone();
            }
        };
        Self {
            request,
            sources,
            row: actual,
            cell,
            authority,
        }
    }

    fn next(&mut self, doc: &MarkdownDocumentBuffer) -> MarkdownViewportSourceRequiredRowResult {
        let query = self
            .sources
            .required_row_query_for(
                doc,
                self.authority,
                &self.row,
                MarkdownTableWindowNavigation::Next,
            )
            .unwrap()
            .unwrap();
        self.request
            .continue_forward_with_required_row(doc, &self.sources, 0..1, self.authority, query)
            .unwrap();
        self.sources = finish(&mut self.request, doc, self.authority);
        let result = self
            .sources
            .required_row_result_for(doc, self.authority)
            .unwrap()
            .unwrap();
        self.row = match &result {
            MarkdownViewportSourceRequiredRowResult::Row { row } => row.clone(),
            MarkdownViewportSourceRequiredRowResult::TableEnd { terminal } => terminal.clone(),
        };
        result
    }

    fn terminal(&mut self, doc: &MarkdownDocumentBuffer) -> VerifiedMarkdownTableRowSource {
        let MarkdownViewportSourceRequiredRowResult::TableEnd { terminal } = self.next(doc) else {
            panic!("当前行后仍有真实成员，不是可追加的 terminal")
        };
        terminal
    }
}

#[test]
fn real_terminal_and_raw_draft_preserve_eol_containers_implicit_columns_and_one_undo() {
    for (source, row, column, input, expected) in [
        (
            SOURCE,
            1,
            1,
            "  新|中\r\n😀  ",
            "| H | C |\n| --- | --- |\n| a | 新\\|中 😀 |\n| | |\n",
        ),
        (
            "| H | C |\r\n| - | - |\r\n| a | b |\r\n\r\n后继正文😀\r\n",
            1,
            1,
            "候选",
            "| H | C |\r\n| - | - |\r\n| a | 候选 |\r\n| | |\r\n\r\n后继正文😀\r\n",
        ),
        (
            "> > | H | C |\r\n> > | - | - |\r\n> > | a | b |",
            1,
            1,
            "候选",
            "> > | H | C |\r\n> > | - | - |\r\n> > | a | 候选 |\r\n> > | | |",
        ),
        (
            "3. | H | C |\r\n   | - | - |",
            0,
            1,
            "候选",
            "3. | H | 候选 |\r\n   | - | - |\r\n   | | |",
        ),
        (
            "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a|\n",
            1,
            3,
            "新😀",
            "| H1 | H2 | H3 | H4 |\n| - | - | - | - |\n|a| |  | 新😀\n| | | | |\n",
        ),
    ] {
        let (_directory, mut doc) = document(source);
        let mapping = doc.original_backing_observer().unwrap();
        let mut origin = CellOrigin::new(&doc, row, column);
        let plan = doc
            .plan_verified_table_cell_draft(origin.cell.clone())
            .unwrap();
        let token = doc.begin_prepared_cell_draft(plan).unwrap();
        replace(&mut doc, &token, input);
        let observer = doc.cell_draft_resource_observer(&token).unwrap();
        // 活动格的 canonical 来源尚未改变；同一个 Source 请求现在才确认后继不存在及真正 End。
        let terminal = origin.terminal(&doc);
        let prepared = doc
            .prepare_cell_draft_row_append(
                doc.capture_cell_draft_edit(&token).unwrap(),
                &terminal,
                origin.authority,
                None,
            )
            .unwrap();
        assert_eq!(prepared.next_cell(), Some((row + 1, 0)));
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .edit
            .unwrap();
        assert_eq!(canonical(&doc), expected);
        assert_eq!(std::fs::read(doc.path()).unwrap(), source.as_bytes());
        assert!(observer.is_released());
        assert!(terminal.coordinate_for(&doc, origin.authority).is_err());
        assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_some());
        assert_eq!(canonical(&doc), source);
        assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_none());
        assert!(doc.redo_text_edit_observed(NoopObserver).unwrap().is_some());
        assert_eq!(canonical(&doc), expected);
        origin.request.cancel();
        drop(doc);
        assert!(mapping.unmap_succeeded());
        assert!(!mapping.failed());
    }
}

#[test]
fn nonterminal_wrong_last_column_foreign_and_undo_stale_end_keep_the_original_draft() {
    let source = "| H | C |\n| - | - |\n| a | b |\n| c | d |\n";
    let (_directory, mut doc) = document(source);
    let mut first = CellOrigin::new(&doc, 1, 1);
    let plan = doc
        .plan_verified_table_cell_draft(first.cell.clone())
        .unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    replace(&mut doc, &token, "原草稿");
    let MarkdownViewportSourceRequiredRowResult::Row { row } = first.next(&doc) else {
        panic!("第二行是成员")
    };
    assert!(matches!(
        doc.prepare_cell_draft_row_append(
            doc.capture_cell_draft_edit(&token).unwrap(),
            &row,
            first.authority,
            None,
        ),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::ContextPending
        ))
    ));
    assert_eq!(read(&doc.capture_cell_draft(&token).unwrap()), "原草稿");
    assert!(doc.cancel_cell_draft(&token));

    let mut last = CellOrigin::new(&doc, 2, 0);
    let terminal = last.terminal(&doc);
    let plan = doc
        .plan_verified_table_cell_draft(last.cell.clone())
        .unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    assert!(matches!(
        doc.prepare_cell_draft_row_append(
            doc.capture_cell_draft_edit(&token).unwrap(),
            &terminal,
            last.authority,
            None,
        ),
        Err(MarkdownCellDraftError::Table(
            MarkdownTableWindowError::InvalidSource
        ))
    ));
    assert!(doc.cancel_cell_draft(&token));

    let mut current = CellOrigin::new(&doc, 2, 1);
    let plan = doc
        .plan_verified_table_cell_draft(current.cell.clone())
        .unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    let terminal = current.terminal(&doc);
    let (_other_directory, other) = document(source);
    let mut foreign = CellOrigin::new(&other, 2, 1);
    let foreign_terminal = foreign.terminal(&other);
    assert!(matches!(
        doc.prepare_cell_draft_row_append(
            doc.capture_cell_draft_edit(&token).unwrap(),
            &foreign_terminal,
            foreign.authority,
            None,
        ),
        Err(MarkdownCellDraftError::Stale)
    ));
    assert!(doc.cancel_cell_draft(&token));
    // Undo 恢复字节后旧 End 仍不复活；用新原格激活，再把旧 terminal 交真实生产入口验证拒绝。
    doc.insert_utf16(0, "改", MarkdownTextSelection::caret(1))
        .unwrap();
    doc.undo_text_edit_observed(NoopObserver).unwrap().unwrap();
    let current = CellOrigin::new(&doc, 2, 1);
    let plan = doc
        .plan_verified_table_cell_draft(current.cell.clone())
        .unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    assert!(matches!(
        doc.prepare_cell_draft_row_append(
            doc.capture_cell_draft_edit(&token).unwrap(),
            &terminal,
            current.authority,
            None,
        ),
        Err(MarkdownCellDraftError::Stale)
    ));
    assert_eq!(canonical(&doc), source);
    assert_eq!(std::fs::read(doc.path()).unwrap(), source.as_bytes());
    assert!(doc.cancel_cell_draft(&token));
}

#[test]
fn marked_text_and_live_readers_reject_append_without_destroying_the_draft() {
    let (_directory, mut doc) = document(SOURCE);
    let mut origin = CellOrigin::new(&doc, 1, 1);
    let plan = doc
        .plan_verified_table_cell_draft(origin.cell.clone())
        .unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    replace(&mut doc, &token, "候选");
    let terminal = origin.terminal(&doc);
    let first = doc.capture_cell_draft(&token).unwrap();
    let second = doc.capture_cell_draft(&token).unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let prepared = doc
        .prepare_cell_draft_row_append(
            doc.capture_cell_draft_edit(&token).unwrap(),
            &terminal,
            origin.authority,
            None,
        )
        .unwrap();
    assert!(matches!(
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver),
        Err(MarkdownCellDraftCommitError::Draft(
            MarkdownCellDraftError::ReadersBusy
        ))
    ));
    assert_eq!(read(&first), "候选");
    assert_eq!(read(&second), "候选");
    assert_eq!(canonical(&doc), SOURCE);
    drop((first, second));
    edit(
        &mut doc,
        &token,
        0..0,
        "",
        MarkdownTextSelection::caret(0),
        Some(0..1),
    )
    .unwrap();
    assert!(matches!(
        doc.prepare_cell_draft_row_append(
            doc.capture_cell_draft_edit(&token).unwrap(),
            &terminal,
            origin.authority,
            None,
        ),
        Err(MarkdownCellDraftError::CompositionActive)
    ));
    assert_eq!(canonical(&doc), SOURCE);
    assert!(doc.cancel_cell_draft(&token));
    assert!(observer.is_released());
}

#[test]
fn two_mib_original_last_cell_and_appended_row_form_one_streamed_canonical_transaction() {
    use std::io::{Read, Seek, SeekFrom, Write};
    const PREFIX: &str = "| H | C |\r\n| --- | --- |\r\n| left | ";
    const TAIL: &str = "中😀 |\r\n";
    const BODY_BYTES: usize = 2 * 1024 * 1024;
    let ascii = BODY_BYTES - "中😀".len();
    let page = [b'a'; 4096];
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(PREFIX.as_bytes()).unwrap();
    for _ in 0..ascii / page.len() {
        file.write_all(&page).unwrap();
    }
    file.write_all(&page[..ascii % page.len()]).unwrap();
    file.write_all(TAIL.as_bytes()).unwrap();
    file.flush().unwrap();
    let mut doc = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    let mapping = doc.original_backing_observer().unwrap();
    let mut origin = CellOrigin::new(&doc, 1, 1);
    let plan = doc
        .plan_verified_table_cell_draft(origin.cell.clone())
        .unwrap();
    let token = doc.begin_prepared_cell_draft(plan).unwrap();
    let facts = doc.cell_draft_facts(&token).unwrap();
    assert_eq!(facts.byte_len, BODY_BYTES);
    edit(
        &mut doc,
        &token,
        facts.utf16_len..facts.utf16_len,
        "尾",
        MarkdownTextSelection::caret(facts.utf16_len + 1),
        None,
    )
    .unwrap();
    // 真实超长原格反向选择跨过两MiB正文；追加的新行不应把原末端投到下一格。
    doc.set_cell_draft_selection(
        doc.capture_cell_draft_edit(&token).unwrap(),
        MarkdownTextSelection {
            anchor_utf16: facts.utf16_len + 1,
            head_utf16: ascii,
        },
    )
    .unwrap();
    let observer = doc.cell_draft_resource_observer(&token).unwrap();
    let terminal = origin.terminal(&doc);
    let prepared = doc
        .prepare_cell_draft_row_append(
            doc.capture_cell_draft_edit(&token).unwrap(),
            &terminal,
            origin.authority,
            Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: ascii + 1,
                    head_utf16: facts.utf16_len + 1,
                },
                origin_utf16: ascii + 1,
            }),
        )
        .unwrap();
    assert_eq!(prepared.next_cell(), Some((2, 0)));
    let committed = doc
        .commit_prepared_cell_draft_observed(prepared, NoopObserver)
        .unwrap();
    assert!(committed.edit.is_some());
    assert_eq!(committed.receipt.next_cell, Some((2, 0)));
    assert_eq!(
        committed.receipt.word_anchor,
        Some(MarkdownCellDraftWordAnchor {
            range: MarkdownTextSelection {
                anchor_utf16: PREFIX.len() + ascii + 1,
                head_utf16: PREFIX.len() + ascii + 4
            },
            origin_utf16: PREFIX.len() + ascii + 1
        })
    );
    assert_eq!(
        committed.receipt.selection,
        MarkdownTextSelection {
            anchor_utf16: PREFIX.len() + ascii + 4,
            head_utf16: PREFIX.len() + ascii,
        }
    );
    assert!(observer.is_released());
    // 固定前缀、每个真实 ASCII 单元和 Unicode/新行尾分别核对，不生成整格测试镜像。
    let assert_text = |doc: &MarkdownDocumentBuffer, tail: &str| {
        let mut units = [0; 2048];
        doc.copy_utf16_code_units(0, &mut units[..PREFIX.len()])
            .unwrap();
        assert!(
            units[..PREFIX.len()]
                .iter()
                .copied()
                .eq(PREFIX.encode_utf16())
        );
        let mut at = PREFIX.len();
        while at < PREFIX.len() + ascii {
            let count = units.len().min(PREFIX.len() + ascii - at);
            doc.copy_utf16_code_units(at, &mut units[..count]).unwrap();
            assert!(units[..count].iter().all(|unit| *unit == u16::from(b'a')));
            at += count;
        }
        let length = tail.encode_utf16().count();
        doc.copy_utf16_code_units(at, &mut units[..length]).unwrap();
        assert!(units[..length].iter().copied().eq(tail.encode_utf16()));
        assert_eq!(doc.utf16_len(), at + length);
    };
    assert_text(&doc, "中😀尾 |\r\n| | |\r\n");
    assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_some());
    assert_text(&doc, TAIL);
    assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_none());
    assert!(doc.redo_text_edit_observed(NoopObserver).unwrap().is_some());
    assert_text(&doc, "中😀尾 |\r\n| | |\r\n");
    // canonical 编辑没有写盘；原文件的每个字节仍用固定页逐段验证。
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut disk = [0; 4096];
    file.read_exact(&mut disk[..PREFIX.len()]).unwrap();
    assert_eq!(&disk[..PREFIX.len()], PREFIX.as_bytes());
    let mut remaining = ascii;
    while remaining != 0 {
        let count = disk.len().min(remaining);
        file.read_exact(&mut disk[..count]).unwrap();
        assert_eq!(&disk[..count], &page[..count]);
        remaining -= count;
    }
    file.read_exact(&mut disk[..TAIL.len()]).unwrap();
    assert_eq!(&disk[..TAIL.len()], TAIL.as_bytes());
    assert_eq!(file.read(&mut disk[..1]).unwrap(), 0);
    let backings: Vec<_> = doc.backing_observers().collect();
    origin.request.cancel();
    drop(doc);
    assert!(mapping.unmap_succeeded());
    assert!(!mapping.failed());
    for backing in backings {
        assert!(backing.unmap_succeeded());
        assert_eq!(backing.mapped_bytes(), 0);
        assert!(!backing.failed());
    }
}

#[test]
fn original_end_plan_streams_large_padding_and_preserves_each_actual_eol() {
    use std::io::{Read, Seek, SeekFrom, Write};
    const PREFIX: &str = "| H | C |\n| --- | --- |\n| a | ";
    const PADDING: usize = 80_000;
    let page: [u8; 4096] = std::array::from_fn(|index| if index % 2 == 0 { b' ' } else { b'\t' });
    for eol in ["\n", "\r\n", "\r", ""] {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(PREFIX.as_bytes()).unwrap();
        file.write_all("原".as_bytes()).unwrap();
        for _ in 0..PADDING / page.len() {
            file.write_all(&page).unwrap();
        }
        file.write_all(&page[..PADDING % page.len()]).unwrap();
        file.write_all(format!("|{eol}").as_bytes()).unwrap();
        file.flush().unwrap();
        let mut doc = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
        let mapping = doc.original_backing_observer().unwrap();
        // 原单格请求已经实际取得 End 时，直接消费它；不为兼容测试构造窗口或第二份来源。
        let plan = super::original_source::plan(&doc, 1, 1);
        let token = doc.begin_prepared_cell_draft(plan).unwrap();
        assert_eq!(doc.cell_draft_facts(&token).unwrap().byte_len, "原".len());
        replace(&mut doc, &token, "改");
        let observer = doc.cell_draft_resource_observer(&token).unwrap();
        let prepared = doc
            .prepare_cell_draft_original_row_append(
                doc.capture_cell_draft_edit(&token).unwrap(),
                None,
            )
            .unwrap();
        doc.commit_prepared_cell_draft_observed(prepared, NoopObserver)
            .unwrap()
            .edit
            .unwrap();
        assert!(observer.is_released());
        // 尾部 padding 不属于 raw 格正文；追加必须逐片保留它，而非重新 trim 整个原物理行。
        let assert_text = |doc: &MarkdownDocumentBuffer, glyph: &str, tail: &str| {
            let mut units = [0; 2048];
            let prefix_len = PREFIX.len() + 1;
            doc.copy_utf16_code_units(0, &mut units[..prefix_len])
                .unwrap();
            assert!(
                units[..prefix_len]
                    .iter()
                    .copied()
                    .eq(PREFIX.encode_utf16().chain(glyph.encode_utf16()))
            );
            let mut at = 0;
            while at < PADDING {
                let count = units.len().min(PADDING - at);
                doc.copy_utf16_code_units(prefix_len + at, &mut units[..count])
                    .unwrap();
                for (offset, unit) in units[..count].iter().enumerate() {
                    assert_eq!(*unit, u16::from(page[(at + offset) % page.len()]));
                }
                at += count;
            }
            let length = tail.encode_utf16().count();
            doc.copy_utf16_code_units(prefix_len + PADDING, &mut units[..length])
                .unwrap();
            assert!(units[..length].iter().copied().eq(tail.encode_utf16()));
            assert_eq!(doc.utf16_len(), prefix_len + PADDING + length);
        };
        let after = format!("|{}| | |{eol}", if eol.is_empty() { "\n" } else { eol });
        assert_text(&doc, "改", &after);
        assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_some());
        assert_text(&doc, "原", &format!("|{eol}"));
        assert!(doc.undo_text_edit_observed(NoopObserver).unwrap().is_none());
        assert!(doc.redo_text_edit_observed(NoopObserver).unwrap().is_some());
        assert_text(&doc, "改", &after);
        // 同一源文件逐页核对原字节；编辑、Undo/Redo 没有偷偷保存或置换原磁盘文件。
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut disk = [0; 4096];
        file.read_exact(&mut disk[..PREFIX.len()]).unwrap();
        assert_eq!(&disk[..PREFIX.len()], PREFIX.as_bytes());
        file.read_exact(&mut disk[.."原".len()]).unwrap();
        assert_eq!(&disk[.."原".len()], "原".as_bytes());
        let mut remaining = PADDING;
        while remaining != 0 {
            let count = disk.len().min(remaining);
            file.read_exact(&mut disk[..count]).unwrap();
            assert_eq!(&disk[..count], &page[..count]);
            remaining -= count;
        }
        let tail = format!("|{eol}");
        file.read_exact(&mut disk[..tail.len()]).unwrap();
        assert_eq!(&disk[..tail.len()], tail.as_bytes());
        assert_eq!(file.read(&mut disk[..1]).unwrap(), 0);
        let backings: Vec<_> = doc.backing_observers().collect();
        drop(doc);
        assert!(mapping.unmap_succeeded());
        assert!(!mapping.failed());
        for backing in backings {
            assert!(backing.unmap_succeeded());
            assert_eq!(backing.mapped_bytes(), 0);
            assert!(!backing.failed());
        }
    }
}
