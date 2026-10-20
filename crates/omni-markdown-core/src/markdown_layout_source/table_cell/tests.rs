//! 用原窗口、原长格 Scanner 和真实文件核对只读来源；不生成产品校验层或整格镜像。

use super::*;
use crate::MarkdownTextSelection;
use crate::markdown_document_buffer::{
    MarkdownTableCellEditStep, MarkdownTableWindowStep, MarkdownTableWindowTarget,
    VerifiedMarkdownTableWindow,
};
use crate::markdown_viewport_presentation::table_window::MarkdownTableWindowCellSource;

const SMALL: &str = "| H | C | E | M |\r\n| --- | --- | --- | --- |\r\n| `a\\|b` | 中😀 ||\r\n";

/// 小样本只用于和原完整有限 Parser 比较，源文件与源代次均由原文档管理。
fn document(text: &str) -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), text).unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    (file, document)
}

/// 只沿现原表格请求取真实 Ready，不能手工构造窗口来给任意范围补票。
fn window(document: &MarkdownDocumentBuffer) -> VerifiedMarkdownTableWindow {
    let mut request = document
        .table_window_request(
            MarkdownTableWindowTarget::AtUtf16(0),
            0..document.utf16_len(),
            1,
            0..0,
        )
        .unwrap();
    loop {
        match request.advance(document, false, 4096).unwrap() {
            MarkdownTableWindowStep::Progress { .. } => {}
            MarkdownTableWindowStep::Ready { .. } => {
                return request.take_ready_for(document).unwrap().unwrap();
            }
            other => panic!("原有限表格没有真实 Ready：{other:?}"),
        }
    }
}

/// 巨格与普通格共用原 4 KiB Scanner；此返回值只证明 raw 来源，不签预览语义。
fn scanned_cell(
    document: &MarkdownDocumentBuffer,
    row: usize,
    column: usize,
) -> VerifiedMarkdownTableCellSource {
    let mut request = document.table_cell_edit_request(0, row, column).unwrap();
    loop {
        let step = request.advance(document, false, 4096).unwrap();
        let work = match step {
            MarkdownTableCellEditStep::Progress { work }
            | MarkdownTableCellEditStep::Ready { work } => work,
            other => panic!("原格结构未完成：{other:?}"),
        };
        assert!(work.source_bytes <= 4096); // 真正读取预算，不能用原格长度代替本步实际读取。
        if matches!(step, MarkdownTableCellEditStep::Ready { .. }) {
            let cell = request.take_cell_source_for(document).unwrap().unwrap();
            assert!(request.take_plan_for(document).unwrap().is_none());
            return cell; // 原请求只消费一次；布局取走后不能再取得编辑计划。
        }
    }
}

#[test]
/// 表列方向和空/缺格都沿真实公开来源发布；有限原 Parser 的 Tag::Table 是独立语法参考。
fn streamed_alignment_reaches_real_cells_including_empty_and_missing_columns() {
    use crate::MarkdownTableAlignment;
    use pulldown_cmark::{Alignment, Event, Options, Parser, Tag};

    for prefix in ["", "> > ", "   "] {
        let source = format!(
            "{prefix}| L | C | R | D |\r\n{prefix}| :--- | :---: | ---: | --- |\r\n{prefix}| 左 | 中 ||\r\n"
        );
        let expected = Parser::new_ext(&source, Options::ENABLE_TABLES)
            .find_map(|event| match event {
                Event::Start(Tag::Table(alignment)) => Some(alignment),
                _ => None,
            })
            .expect("原 Parser 应识别这张固定短表");
        assert_eq!(
            expected,
            [
                Alignment::Left,
                Alignment::Center,
                Alignment::Right,
                Alignment::None
            ]
        );
        let (file, document) = document(&source);
        let publication = window(&document);
        for row in 0..=1 {
            for (column, alignment) in expected.iter().enumerate() {
                let expected = match alignment {
                    Alignment::None | Alignment::Left => MarkdownTableAlignment::Left,
                    Alignment::Center => MarkdownTableAlignment::Center,
                    Alignment::Right => MarkdownTableAlignment::Right,
                };
                let cell = scanned_cell(&document, row, column);
                let original = document
                    .verified_table_cell_source(&publication, row, column)
                    .unwrap();
                assert_eq!(cell.alignment_for(&document).unwrap(), expected);
                assert_eq!(original.alignment_for(&document).unwrap(), expected);
                assert_eq!(cell.source_for(&document), original.source_for(&document));
                let plan = document.plan_verified_table_cell_draft(cell).unwrap();
                assert_eq!(plan.alignment_for(&document).unwrap(), expected);
            }
        }
        assert!(!document.can_undo() && !document.can_redo() && !document.is_dirty());
        assert_eq!(std::fs::read(file.path()).unwrap(), source.as_bytes());
    }
}

/// 同一原 Parser、流式 Scanner 与编辑计划必须给出相同端点，空格和缺格仍有不同结构后果。
#[test]
fn readonly_and_edit_consumers_share_exact_parser_endpoints_and_empty_cell_distinctions() {
    let (_file, document) = document(SMALL);
    let verified = window(&document);
    for column in 0..4 {
        let direct = document
            .verified_table_cell_source(&verified, 1, column)
            .unwrap();
        let scanned = scanned_cell(&document, 1, column);
        let plan = document
            .plan_verified_table_cell_edit(&verified, 1, column, "")
            .unwrap();
        assert_eq!(direct.source_for(&document), scanned.source_for(&document));
        if column < 3 {
            let seed = scanned.inline_seed_for(&document).unwrap().unwrap();
            assert_eq!(seed.table_cell_coordinate(), Some((0, 1, column)));
            assert_eq!(
                seed.kind(),
                pulldown_cmark::global_frontier::InlineBlockKind::TableCell
            );
            assert_eq!(
                seed.source_range(),
                document
                    .byte_offset_for_utf16(scanned.canonical_range_for(&document).unwrap().start)
                    .unwrap()
                    ..document
                        .byte_offset_for_utf16(scanned.canonical_range_for(&document).unwrap().end)
                        .unwrap()
            ); // 原 scanner 的 byte 与原 canonical UTF16 端点一致，不能以 raw 结构状态代替 inline Ready。
        } else {
            assert!(scanned.inline_seed_for(&document).unwrap().is_none()); // 隐式缺格没有可解析的源正文。
        }
        assert_eq!(
            direct.canonical_range_for(&document).unwrap(),
            plan.replacement_for(&document).unwrap().0
        );
        assert_eq!(direct.coordinate_for(&document).unwrap(), (0, 1, column));
        let source = MarkdownLayoutTextSource::capture_table_cell(&document, &direct).unwrap();
        assert_eq!(
            source.domain(),
            MarkdownLayoutTextSourceDomain::ReadOnlyCell
        );
        assert!(source.cell_draft_authority().is_none());
        assert!(source.read_only_cell_source().is_some());
        let mut bytes = Vec::new(); // 仅这个固定小样本的比较允许完整输出，不用于巨格路径。
        source
            .write_utf16_range_to(0..source.utf16_len(), &mut bytes)
            .unwrap();
        if column == 0 {
            assert_eq!(bytes, b"`a\\|b`"); // raw Source 不擅自执行隐藏、转义或 inline 渲染。
        } else if column == 1 {
            assert_eq!(bytes, "中😀".as_bytes());
        } else {
            assert!(bytes.is_empty());
        }
    }
    let explicit = document
        .verified_table_cell_source(&verified, 1, 2)
        .unwrap();
    let missing = document
        .verified_table_cell_source(&verified, 1, 3)
        .unwrap();
    assert!(matches!(
        explicit.source_for(&document).unwrap(),
        MarkdownTableWindowCellSource::Present(range) if range.is_empty()
    ));
    assert!(matches!(
        missing.source_for(&document).unwrap(),
        MarkdownTableWindowCellSource::ImplicitEmpty { .. }
    ));
}

/// 同源单格 seed 接原 Plain/Code 游标；一字节 frame 必须续读，不能把格误当 Root 或短 frame 当 EOF。
#[test]
fn streamed_cell_seed_drives_original_plain_and_code_cursors_at_exact_source_coordinates() {
    use pulldown_cmark::block_frontier::SourceFrame;
    use pulldown_cmark::global_frontier::{
        GlobalSourceAuthority, InlineCodeStep, InlinePlainLimits, InlinePlainStep,
    };
    let (_file, document) = document(SMALL);
    let limits = || InlinePlainLimits {
        max_byte_work: 1_000_000,
        max_transitions: 1_000_000,
    };
    let authority = || GlobalSourceAuthority {
        current_revision: document.generation(),
        cancelled: false,
    };
    let frame = |at: usize| {
        let end = (at + 1).min(SMALL.len());
        SourceFrame {
            source_revision: document.generation(),
            byte_start: at,
            bytes: &SMALL.as_bytes()[at..end],
            real_eof: end == SMALL.len(),
        }
    };
    let plain = scanned_cell(&document, 1, 1);
    let seed = plain.inline_seed_for(&document).unwrap().unwrap();
    let mut cursor = seed
        .plain_cursor(seed.source_range().end, limits())
        .unwrap();
    loop {
        match cursor
            .advance(frame(cursor.next_byte()), authority(), 4096)
            .unwrap()
        {
            InlinePlainStep::Progress { .. } | InlinePlainStep::NeedInput { .. } => {}
            InlinePlainStep::Proven { proof, .. } => {
                assert_eq!(proof.source_range(), seed.source_range());
                assert_eq!(proof.proven_end(), seed.source_range().end);
                break;
            }
            other => panic!("真实普通格不得失去 TableCell 来源：{other:?}"),
        }
    }
    let code = scanned_cell(&document, 1, 0);
    let seed = code.inline_seed_for(&document).unwrap().unwrap();
    let mut cursor = seed
        .code_cursor(seed.source_range().end, limits())
        .unwrap()
        .unwrap();
    let mut proved = false;
    loop {
        match cursor
            .advance(frame(cursor.next_byte()), authority(), 4096)
            .unwrap()
        {
            InlineCodeStep::Progress { .. } | InlineCodeStep::NeedInput { .. } => {}
            InlineCodeStep::Span { proof, .. } => {
                assert_eq!(proof.source_range(), seed.source_range());
                assert_eq!(&SMALL[proof.raw_body_range()], "a\\|b");
                proved = true;
            }
            InlineCodeStep::Proven {
                requires_general_styles,
                ..
            } => {
                assert!(!requires_general_styles);
                assert!(proved);
                break;
            }
            other => panic!("真实代码格必须保留原匹配与相同 byte 范围：{other:?}"),
        }
    }
}

/// 孤立格直接调用原 Active lexer；块标记、pipe/code、强调、引用和数学必须与完整原表相同。
#[test]
fn bounded_cell_parser_preserves_original_table_inline_events_instead_of_root_blocks() {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let source = "|H|C|\n|---|---|\n|# heading|a|\n|- list|a|\n|> quote|a|\n|**bold** *italic* [go][id] $x{y}$|a|\n|`a\\|b`|a|\n\n[id]: /destination \"title\"\n";
    let (_file, document) = document(source);
    let first = scanned_cell(&document, 1, 0);
    let options = first.inline_seed_for(&document).unwrap().unwrap().options();
    let description = |event: Event<'_>| match event {
        // 真外部 callback 按上游协议给出 ReferenceUnknown；语义 oracle 只核原目标和值，不假装内置定义。
        Event::Start(Tag::Link {
            dest_url, title, ..
        }) => format!("link:{dest_url:?}:{title:?}"),
        other => format!("{other:?}"),
    };
    let mut cells = Vec::new();
    let mut active = None;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match event {
            Event::Start(Tag::TableCell) => active = Some(Vec::new()),
            Event::End(TagEnd::TableCell) => cells.push(active.take().unwrap()),
            event => {
                if let Some(cell) = &mut active {
                    cell.push((description(event), range));
                }
            }
        }
    }
    let definitions = Parser::new_ext(source, options);
    for row in 1..=5 {
        let cell = scanned_cell(&document, row, 0);
        let seed = cell.inline_seed_for(&document).unwrap().unwrap();
        let mut callback = |link: pulldown_cmark::BrokenLink<'_>| {
            definitions
                .reference_definitions()
                .get(&link.reference)
                .map(|definition| {
                    (
                        definition.dest.clone(),
                        definition.title.clone().unwrap_or_else(|| "".into()),
                    )
                })
        };
        let parsed = Parser::new_with_table_cell_seed(
            &source[seed.source_range()],
            seed,
            Some(&mut callback),
        )
        .unwrap()
        .into_offset_iter()
        .map(|(event, range)| {
            (
                description(event),
                range.start + seed.source_range().start..range.end + seed.source_range().start,
            )
        })
        .collect::<Vec<_>>();
        assert_eq!(parsed, cells[row * 2], "真实表行 {row}");
    }
}

/// 多个只读格不阻碍唯一编辑格和两个真实 reader，取消草稿也不能撤销 canonical 冻结字节。
#[test]
fn readonly_captures_never_take_the_unique_draft_slot_or_its_two_reader_claims() {
    let (_file, mut document) = document(SMALL);
    let verified = window(&document);
    let cell = document
        .verified_table_cell_source(&verified, 1, 0)
        .unwrap();
    let first = MarkdownLayoutTextSource::capture_table_cell(&document, &cell).unwrap();
    let before = document.storage_profile();
    assert!(!document.has_cell_draft());
    let token = document.begin_cell_draft(&verified, 1, 0).unwrap();
    let draft = document.cell_draft_resource_observer(&token).unwrap();
    let facts = draft.snapshot();
    let second = MarkdownLayoutTextSource::capture_table_cell(&document, &cell).unwrap();
    assert_eq!(draft.snapshot(), facts);
    assert_eq!(facts.read_roots, 0);
    let draft_source = MarkdownLayoutTextSource::capture_cell_draft(&document, &token).unwrap();
    assert_eq!(
        draft_source.domain(),
        MarkdownLayoutTextSourceDomain::ActiveCellDraft
    );
    assert!(draft_source.read_only_cell_source().is_none());
    assert_eq!(document.storage_profile(), before);
    assert!(document.cancel_cell_draft(&token));
    drop(draft_source);
    document.drain_cancelled_cell_draft();
    assert!(draft.is_released());
    first.byte_offset_for_utf16(0).unwrap();
    second.byte_offset_for_utf16(0).unwrap(); // 原格只读来源不属于已取消的局部草稿域。
}

/// 流式写入 2 MiB 格，夹具本身也不创建完整 payload 或完整文档字符串。
fn giant_document() -> (tempfile::NamedTempFile, MarkdownDocumentBuffer) {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"| H | C |\n| --- | --- |\n| ").unwrap();
    let block = [b'x'; 4096];
    for _ in 0..512 {
        file.write_all(&block).unwrap();
    }
    file.write_all("中😀 | other |\n".as_bytes()).unwrap();
    file.flush().unwrap();
    let document = MarkdownDocumentBuffer::open(file.path(), 32 * 1024 * 1024).unwrap();
    (file, document)
}

/// 目标逐片核验内容，不积累巨格；真实 writer 边界同时证实单次借用没有放大。
struct GiantDestination {
    ascii_remaining: usize,
    tail_written: usize,
}
impl Write for GiantDestination {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(bytes.len() <= MARKDOWN_LAYOUT_SOURCE_BYTES);
        let ascii = self.ascii_remaining.min(bytes.len());
        assert!(bytes[..ascii].iter().all(|byte| *byte == b'x'));
        self.ascii_remaining -= ascii;
        let tail = &bytes[ascii..];
        let expected = "中😀".as_bytes();
        assert_eq!(
            tail,
            &expected[self.tail_written..self.tail_written + tail.len()]
        );
        self.tail_written += tail.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 巨格按有限源片完整输出，原活动文档退场后只保字节资源，最后一个布局 Source 退场才解除映射。
#[test]
fn existing_two_mib_raw_cell_streams_from_shared_pieces_and_releases_after_last_source() {
    let (_file, document) = giant_document();
    let before = document.storage_profile();
    let cell = scanned_cell(&document, 1, 0);
    let source = MarkdownLayoutTextSource::capture_table_cell(&document, &cell).unwrap();
    assert_eq!(source.byte_len(), 2 * 1024 * 1024 + "中😀".len());
    assert_eq!(source.utf16_len(), 2 * 1024 * 1024 + 3);
    assert_eq!(document.storage_profile(), before);
    assert!(!document.has_cell_draft());
    assert_eq!(
        source.visit_byte_range(0..4097, |_| Ok(())),
        Err(MarkdownDocumentBufferError::MaterializationLimitExceeded)
    );
    let backing = source.original_backing_observer().unwrap();
    let retired = source.resource_observer();
    drop(document); // 来源不保活动 Doc、Undo 或草稿；原映射仅由当前冻结 slice 持有。
    let mut destination = GiantDestination {
        ascii_remaining: 2 * 1024 * 1024,
        tail_written: 0,
    };
    source
        .write_utf16_range_to(0..source.utf16_len(), &mut destination)
        .unwrap();
    assert_eq!(
        (destination.ascii_remaining, destination.tail_written),
        (0, "中😀".len())
    );
    assert!(!retired.payload_released());
    drop(source);
    assert!(retired.payload_released());
    assert!(backing.unmap_succeeded() && backing.mapped_bytes() == 0 && !backing.failed());
}

/// 冻结字节和当前文档权限分离；另一个文档及 Undo 回到旧内容都不能复活旧格票。
#[test]
fn old_cell_tickets_never_survive_other_documents_edit_generations_or_undo() {
    let (_file, mut original) = document(SMALL);
    let (_other_file, other) = document(SMALL);
    let cell = scanned_cell(&original, 1, 1);
    let source = MarkdownLayoutTextSource::capture_table_cell(&original, &cell).unwrap();
    assert!(matches!(
        MarkdownLayoutTextSource::capture_table_cell(&other, &cell),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    original
        .insert_utf16(0, "前", MarkdownTextSelection::caret(1))
        .unwrap();
    assert!(matches!(
        cell.canonical_range_for(&original),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    original.undo_text_edit().unwrap();
    assert!(matches!(
        MarkdownLayoutTextSource::capture_table_cell(&original, &cell),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    let mut bytes = Vec::new();
    source.write_utf16_range_to(0..3, &mut bytes).unwrap();
    assert_eq!(bytes, "中😀".as_bytes()); // 冻结旧字节仍可读，但绝不让它们取得当前 canonical 票。
}

/// 全编辑片组成的格仍来自原表格，真实来源 EIO 必须优先于同次 writer 失败。
#[test]
fn edited_only_raw_cell_keeps_original_io_failure_ahead_of_writer_failure() {
    struct FailingDestination<'a>(&'a MarkdownDocumentBuffer);
    impl Write for FailingDestination<'_> {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            self.0.fail_next_original_reclaim_for_test();
            assert_eq!(
                self.0.reclaim_original_read_windows_for_test(),
                Err(MarkdownDocumentBufferError::OriginalBackingUnavailable(
                    libc::EIO
                ))
            );
            Err(std::io::Error::from_raw_os_error(libc::EBADF))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (_file, mut document) = document(SMALL);
    let cell = scanned_cell(&document, 1, 1);
    let range = cell.canonical_range_for(&document).unwrap().clone();
    document
        .replace_utf16_range(
            range.clone(),
            "全编辑",
            MarkdownTextSelection::caret(range.start + 3),
        )
        .unwrap();
    let current = scanned_cell(&document, 1, 1);
    let source = MarkdownLayoutTextSource::capture_table_cell(&document, &current).unwrap();
    source.visit_byte_range(0..3, |_| Ok(())).unwrap();
    let error = MarkdownDocumentBufferError::OriginalBackingUnavailable(libc::EIO);
    assert_eq!(
        source.write_utf16_range_to(0..source.utf16_len(), &mut FailingDestination(&document)),
        Err(error)
    );
    assert!(matches!(
        current.source_for(&document),
        Err(MarkdownTableRequestError::Source(actual)) if actual == error
    ));
    assert!(matches!(
        MarkdownLayoutTextSource::capture_table_cell(&document, &current),
        Err(MarkdownTableRequestError::Source(actual)) if actual == error
    ));
}

#[cfg(feature = "paged-shaping-frontier")]
/// 原 Composer 真实产出局部首行与可读 spool；raw 字面排版不伪装成 inline 或网格已完成。
#[test]
fn original_composer_consumes_readonly_raw_cell_with_real_prefix_spool_and_retires_resources() {
    use crate::{
        MarkdownLayoutFont, MarkdownLineComposeAnswer, MarkdownLineComposeBudget,
        MarkdownLineComposeCancellation, MarkdownLineComposeStyle, MarkdownLineComposer,
        MarkdownLineFontCatalog,
    };
    let (_file, document) =
        document("| H | C |\n| --- | --- |\n| hello world hello world hello world | other |\n");
    let cell = scanned_cell(&document, 1, 0);
    let source = MarkdownLayoutTextSource::capture_table_cell(&document, &cell).unwrap();
    let released = source.resource_observer();
    let bytes: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/harfrust/benches/fonts/Roboto-Regular.ttf"
    ));
    let font =
        MarkdownLayoutFont::resolve(std::sync::Arc::new(bytes), "Roboto-Regular", &[]).unwrap();
    let catalog = MarkdownLineFontCatalog::new();
    let registered = catalog.register(&font).unwrap();
    let mut composer = MarkdownLineComposer::new_with_frontier(
        source.clone(),
        &source.authority(),
        catalog,
        80 * 64,
        MarkdownLineComposeCancellation::new(),
    )
    .unwrap();
    let resources = composer.worker_resource_observer();
    let line = loop {
        match composer
            .step(MarkdownLineComposeBudget::default())
            .unwrap()
            .answer
        {
            MarkdownLineComposeAnswer::Yielded => {}
            MarkdownLineComposeAnswer::NeedStyle(request) => {
                assert!(
                    request.source() == &source.authority(),
                    "原只读格的排版请求必须保留同一冻结Source身份"
                );
                let range = request.source_range();
                composer
                    .provide_style(
                        request
                            .respond_registered(
                                range,
                                &registered,
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
            MarkdownLineComposeAnswer::LineReady(line) => break line,
            _ => panic!("原 raw 格首行尚未完成，不能把其他阶段冒充真实 LineEnd"),
        }
    };
    assert_eq!(line.source_utf16.start, 0);
    assert!(line.source_utf16.end > 0 && line.source_utf16.end < source.utf16_len());
    let mut output = composer.into_output().unwrap();
    let published = output.last_line().unwrap().unwrap();
    assert_eq!(published.facts(), &line);
    let part = output.part(&published, 0).unwrap().unwrap();
    output.read_batch(&part, 0..0, &mut []).unwrap();
    output.close().unwrap(); // 真正关闭原 spool FD，而不是只观察 Rust 外壳被移出。
    drop((part, published, registered, font, source, document));
    assert!(resources.is_released());
    assert!(resources.snapshot().unwrap().actual_resources_released());
    assert!(released.payload_released());
}
