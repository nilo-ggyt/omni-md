//! 真实来源出版、原 Style nonce、原字体注册与原输出目录共同验证表格块交接。
use super::*;
use crate::{
    MarkdownDocumentBuffer, MarkdownLayoutFont, MarkdownViewportSourceAuthority,
    MarkdownViewportSourceStep,
};
use std::sync::Arc;

/// 只把同一个原 leaf request 的结构出版物交给生产 row factory，不手工制造 canonical 范围。
fn header() -> (
    tempfile::NamedTempFile,
    MarkdownDocumentBuffer,
    MarkdownLayoutTextSource,
    VerifiedMarkdownLayoutTableRow,
) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "| H | C |\r\n| --- | --- |\r\n").unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 57,
        selection: document.selection(),
        cancelled: false,
    };
    let mut request = document
        .viewport_leaf_source_request(0..document.utf16_len(), 57, authority.selection)
        .unwrap();
    loop {
        if matches!(
            request.advance(&document, authority, 4096).unwrap(),
            MarkdownViewportSourceStep::SourcesAvailable { .. }
        ) {
            let published = request
                .take_sources_for(&document, authority)
                .unwrap()
                .unwrap();
            if let Some(row) = published.table_row_at_for(&document, authority, 0).unwrap() {
                let row = source
                    .verified_table_layout_row_from_source(&document, row, authority)
                    .unwrap();
                return (file, document, source, row);
            }
        }
    }
}

#[test]
fn real_row_waits_for_grid_then_publishes_exact_block_geometry_and_only_two_visible_gcbs() {
    let (_file, _document, source, row) = header();
    let catalog = MarkdownLineFontCatalog::new();
    let font = MarkdownLayoutFont::resolve(
        Arc::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
            ))
            .as_slice(),
        ),
        "Roboto-Regular",
        &[],
    )
    .unwrap();
    let font = catalog.register(&font).unwrap();
    let mut worker = MarkdownLineComposer::new_with_table_flow(
        source.clone(),
        &source.authority(),
        catalog,
        200 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let style = loop {
        if let MarkdownLineComposeAnswer::NeedStyle(request) = worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            break request;
        }
    };
    let request = worker.begin_table_row(&style, row.clone()).unwrap();
    // 未完成格布局只能保同 nonce 请求；原 canonical 游标与输出行数都不前移。
    for _ in 0..3 {
        assert!(matches!(
            worker
                .step(MarkdownLineComposeBudget::default())
                .unwrap()
                .answer,
            MarkdownLineComposeAnswer::NeedsTableRow(_)
        ));
        assert!(worker.publish_ready_prefix().unwrap().is_none());
    }
    worker
        .provide_table_row(
            request
                .respond_registered(&font, 93 * 64, 200 * 64, &[100 * 64, 100 * 64])
                .unwrap(),
        )
        .unwrap();
    let budget = MarkdownLineComposeBudget {
        bytes: 1,
        scalars: 1,
        records: 1,
        metadata_units: 1,
    };
    let line = loop {
        let step = worker.step(budget).unwrap();
        assert!(
            step.bytes <= 1 && step.scalars <= 1 && step.records <= 1 && step.metadata_units <= 1
        );
        if let MarkdownLineComposeAnswer::LineReady(line) = step.answer {
            break line;
        }
    };
    assert_eq!(line.kind, MarkdownComposedLineKind::TableRow);
    assert_eq!(line.source_utf16, row.source_range());
    assert_eq!(
        (line.glyphs, line.x_advance_26_6, line.y_advance_26_6),
        (0, 200 * 64, 93 * 64)
    );
    let mut output = worker.publish_ready_prefix().unwrap().unwrap();
    let handle = output.last_line().unwrap().unwrap();
    let part = output.part(&handle, 0).unwrap().unwrap();
    assert_eq!(part.facts().kind, MarkdownComposedLinePartKind::TableRow);
    assert_eq!(part.facts().points, 0.0);
    let mut widths = [0; 256];
    assert_eq!(
        output
            .table_column_widths(&source.authority(), &handle, &mut widths)
            .unwrap(),
        2
    );
    assert_eq!(&widths[..2], &[100 * 64, 100 * 64]);
    assert_eq!(
        output.grapheme_ordinal_domain().unwrap(),
        crate::MarkdownGraphemeOrdinalDomain::TableVisible
    );
    assert_eq!(output.grapheme_ordinal(0).unwrap(), Some(0));
    assert_eq!(
        output.grapheme_ordinal(source.utf16_len()).unwrap(),
        Some(1)
    );
    assert!(matches!(
        output.grapheme_ordinal(2),
        Err(MarkdownLineComposeError::TableRange)
    ));
    assert_eq!(output.grapheme_ordinal(0).unwrap(), Some(0));
}

#[test]
fn real_table_between_text_and_blank_crlf_resumes_body_before_and_after_style_reset() {
    let text = "before-table\r\n\r\n| H | C |\r\n| --- | --- |\r\n| a | b |\r\n| c | d |\r\n| e | f |\r\n\r\nafter-table";
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), text).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 57,
        selection: document.selection(),
        cancelled: false,
    };
    let mut sources = document
        .viewport_leaf_source_request(0..document.utf16_len(), 57, authority.selection)
        .unwrap();
    let published = loop {
        if matches!(
            sources.advance(&document, authority, 4096).unwrap(),
            MarkdownViewportSourceStep::SourcesAvailable { .. }
        ) {
            break sources
                .take_sources_for(&document, authority)
                .unwrap()
                .unwrap();
        }
    };
    let catalog = MarkdownLineFontCatalog::new();
    let font = MarkdownLayoutFont::resolve(
        Arc::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
            ))
            .as_slice(),
        ),
        "Roboto-Regular",
        &[],
    )
    .unwrap();
    let font = catalog.register(&font).unwrap();
    let mut worker = MarkdownLineComposer::new_with_table_flow(
        source.clone(),
        &source.authority(),
        catalog.clone(),
        200 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let mut original_lines = Vec::new();
    for pass in 0..2 {
        if pass != 0 {
            // 第一遍的 prefix 已在本轮末端归还；同 Source、原语言 owner 与原结构目录参与真实重排。
            worker.reset_styles(catalog.clone()).unwrap();
        }
        let resources = worker.worker_resource_observer();
        let budget = MarkdownLineComposeBudget {
            bytes: 1,
            scalars: 1,
            records: 1,
            metadata_units: 1,
        };
        let mut rows = 0;
        let mut completed_end = 0;
        let mut lines = Vec::new();
        loop {
            let step = worker.step(budget).unwrap();
            assert!(
                step.bytes <= 1
                    && step.scalars <= 1
                    && step.records <= 1
                    && step.metadata_units <= 1
            );
            match step.answer {
                MarkdownLineComposeAnswer::NeedStyle(request) => {
                    if let Some(row) = published
                        .table_row_at_for(&document, authority, request.source_range().start)
                        .unwrap()
                    {
                        let row = source
                            .verified_table_layout_row_from_source(&document, row, authority)
                            .unwrap();
                        if pass != 0 {
                            // 已知结构行只有原行握手权限；普通字体响应必须明确拒绝，不能补造脚本/级别。
                            assert!(matches!(
                                request.clone().respond_registered(
                                    request.source_range(),
                                    &font,
                                    MarkdownLineComposeStyle {
                                        presentation: Default::default(),
                                        points: 16.0,
                                        language: None,
                                        features: Vec::new(),
                                    },
                                ),
                                Err(MarkdownLineComposeError::TableRange)
                            ));
                        }
                        worker.begin_table_row(&request, row).unwrap();
                    } else {
                        let range = request.source_range();
                        worker
                            .provide_style(
                                request
                                    .respond_registered(
                                        range,
                                        &font,
                                        MarkdownLineComposeStyle {
                                            presentation: Default::default(),
                                            points: 16.0,
                                            language: None,
                                            features: Vec::new(),
                                        },
                                    )
                                    .unwrap(),
                            )
                            .unwrap();
                    }
                }
                MarkdownLineComposeAnswer::NeedsTableRow(request) => {
                    worker
                        .provide_table_row(
                            request
                                .respond_registered(&font, 48 * 64, 200 * 64, &[100 * 64, 100 * 64])
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineComposeAnswer::NeedsControl(request) => {
                    worker
                        .provide_control(
                            request
                                .respond(
                                    &font,
                                    16.0,
                                    MarkdownLineParagraphStyle {
                                        tab_stops: Vec::new(),
                                        default_tab_interval_26_6: None,
                                    },
                                )
                                .unwrap(),
                        )
                        .unwrap();
                }
                MarkdownLineComposeAnswer::LineReady(line) => {
                    assert_eq!(line.source_utf16.start, completed_end);
                    completed_end = line.source_utf16.end;
                    rows += usize::from(line.kind == MarkdownComposedLineKind::TableRow);
                    lines.push(line);
                }
                MarkdownLineComposeAnswer::Complete => break,
                MarkdownLineComposeAnswer::Yielded => (),
                MarkdownLineComposeAnswer::NeedsResolution(_)
                | MarkdownLineComposeAnswer::NeedsFrontier(_)
                | MarkdownLineComposeAnswer::NeedsTabPlacement(_) => {
                    panic!("真实简单正文不需要其他 consumer")
                }
            }
        }
        assert_eq!(rows, 4);
        assert_eq!(completed_end, source.utf16_len());
        if pass == 0 {
            original_lines = lines;
        } else {
            // 对比两次真实输出的来源覆盖、断行种类、字形和几何，不把静态快照或私态当验收。
            assert_eq!(lines, original_lines);
        }
        let mut output = worker.publish_ready_prefix().unwrap().unwrap();
        let mut line = output.last_line().unwrap().unwrap();
        while line.facts().kind != MarkdownComposedLineKind::TableRow {
            line = output.previous_line(&line).unwrap().unwrap();
        }
        let mut widths = [0; 256];
        assert_eq!(
            output
                .table_column_widths(&source.authority(), &line, &mut widths)
                .unwrap(),
            2
        );
        assert_eq!(&widths[..2], &[100 * 64, 100 * 64]);
        let active = resources.snapshot().unwrap().table_directory.unwrap();
        assert!(active.directory_created && !active.owner_released && active.file_open);
        assert_eq!(
            active.buffer_bytes,
            crate::markdown_layout_index::MARKDOWN_LAYOUT_INDEX_BYTES
        );
        if pass == 0 {
            drop(output);
            continue;
        }
        drop(worker);
        let retained = resources.snapshot().unwrap().table_directory.unwrap();
        // 原 writer 已退出并归还唯一页；输出只持已 flush 的真实只读前缀 FD。
        assert!(!retained.owner_released && retained.file_open);
        assert_eq!(retained.buffer_bytes, 0);
        drop(output);
        let released = resources.snapshot().unwrap().table_directory.unwrap();
        assert!(
            released.owner_released
                && !released.file_open
                && released.buffer_bytes == 0
                && released.mapped_bytes == 0
                && !released.cleanup_failed
                && !released.reader_failed
        );
        break; // 原 worker 已真实释放，不能进入另一次布局。
    }
}

#[test]
fn real_forward_publications_cross_structure_page_and_release_original_directory_fd() {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"| H | C |\r\n| --- | --- |\r\n").unwrap();
    for _ in 0..130 {
        file.write_all(b"| a | b |\r\n").unwrap();
    }
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 65536).unwrap();
    let source = MarkdownLayoutTextSource::capture(&document);
    let authority = MarkdownViewportSourceAuthority {
        context_revision: 57,
        selection: document.selection(),
        cancelled: false,
    };
    let mut sources = document
        .viewport_leaf_source_request(0..1, 57, authority.selection)
        .unwrap();
    let mut published = loop {
        if matches!(
            sources.advance(&document, authority, 4096).unwrap(),
            MarkdownViewportSourceStep::SourcesAvailable { .. }
        ) {
            break sources
                .take_sources_for(&document, authority)
                .unwrap()
                .unwrap();
        }
    };
    let catalog = MarkdownLineFontCatalog::new();
    let font = MarkdownLayoutFont::resolve(
        Arc::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
            ))
            .as_slice(),
        ),
        "Roboto-Regular",
        &[],
    )
    .unwrap();
    let font = catalog.register(&font).unwrap();
    let mut worker = MarkdownLineComposer::new_with_table_flow(
        source.clone(),
        &source.authority(),
        catalog,
        200 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let resources = worker.worker_resource_observer();
    let mut rows = 0;
    loop {
        match worker
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                let position = request.source_range().start;
                if !published
                    .requested_for(&document, authority)
                    .unwrap()
                    .contains(&position)
                {
                    // 真实后继行只续同一个原 SourceRequest/Global，不用复制旧出版物构造下一页权限。
                    sources
                        .continue_forward(&document, &published, position..position + 1, authority)
                        .unwrap();
                    published = loop {
                        if matches!(
                            sources.advance(&document, authority, 4096).unwrap(),
                            MarkdownViewportSourceStep::SourcesAvailable { .. }
                        ) {
                            break sources
                                .take_sources_for(&document, authority)
                                .unwrap()
                                .unwrap();
                        }
                    };
                }
                let row = published
                    .table_row_at_for(&document, authority, position)
                    .unwrap()
                    .unwrap();
                let row = source
                    .verified_table_layout_row_from_source(&document, row, authority)
                    .unwrap();
                worker.begin_table_row(&request, row).unwrap();
            }
            MarkdownLineComposeAnswer::NeedsTableRow(request) => {
                worker
                    .provide_table_row(
                        request
                            .respond_registered(&font, 48 * 64, 200 * 64, &[100 * 64, 100 * 64])
                            .unwrap(),
                    )
                    .unwrap();
            }
            MarkdownLineComposeAnswer::LineReady(line) => {
                assert_eq!(line.kind, MarkdownComposedLineKind::TableRow);
                rows += 1;
            }
            MarkdownLineComposeAnswer::Complete => break,
            MarkdownLineComposeAnswer::Yielded => (),
            _ => panic!("真实全表结构不应进入正文 glyph/其他 consumer"),
        }
    }
    assert_eq!(rows, 131);
    let mut output = worker.publish_ready_prefix().unwrap().unwrap();
    let line = output.last_line().unwrap().unwrap();
    let mut widths = [0; 256];
    assert_eq!(
        output
            .table_column_widths(&source.authority(), &line, &mut widths)
            .unwrap(),
        2
    );
    let active = resources.snapshot().unwrap().table_directory.unwrap();
    assert!(active.directory_created && !active.owner_released && active.file_open);
    assert_eq!(
        active.buffer_bytes,
        crate::markdown_layout_index::MARKDOWN_LAYOUT_INDEX_BYTES
    );
    drop(worker);
    let retained = resources.snapshot().unwrap().table_directory.unwrap();
    assert!(!retained.owner_released && retained.file_open);
    assert_eq!(retained.buffer_bytes, 0); // 跨页目录也只保原不可变文件，不保活 writer 的工作页。
    // 内部位置由格显示源负责；输出读者活着时既拒绝 raw 内部 Unicode 权限，也保留原真实目录。
    assert!(matches!(
        output.grapheme_ordinal(line.facts().source_utf16.start + 2),
        Err(MarkdownLineComposeError::TableRange)
    ));
    drop(output);
    let released = resources.snapshot().unwrap().table_directory.unwrap();
    assert!(
        released.owner_released
            && !released.file_open
            && released.buffer_bytes == 0
            && released.mapped_bytes == 0
            && !released.cleanup_failed
            && !released.reader_failed
    );
}
