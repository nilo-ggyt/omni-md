//! 合成真实文件与完整 Parser 是测试 oracle；正文副本和事件 Vec 不进入产品请求。

use super::*;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("普通来源验收.md");
    std::fs::write(&path, source).unwrap();
    let document = MarkdownDocumentBuffer::open(&path, 32 * 1024 * 1024).unwrap();
    (directory, document)
}

fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
    MarkdownViewportSourceAuthority {
        context_revision: 41,
        selection: document.selection(),
        cancelled: false,
    }
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
        match request.advance(document, authority(document), 4096)? {
            MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work } => {
                assert!(work.source_bytes <= 4096);
                assert!(work.structural_transitions <= 4096);
                assert!(work.metadata_bytes <= 4096);
            }
        }
        if request
            .sources_for(document, authority(document))?
            .is_some()
        {
            return Ok(());
        }
    }
    panic!("有限真实来源夹具没有结束");
}

fn units(source: &str, byte: usize) -> usize {
    source[..byte].encode_utf16().count()
}

fn ordinary_text_ranges(source: &str) -> Vec<Range<usize>> {
    let options =
        crate::markdown_viewport_presentation::parser::markdown_presentation_options(true);
    let mut table = false;
    let mut code = false;
    let mut atx = false;
    let mut ranges = Vec::new();
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        // 仅用有限无 inline markup 样本，使真实 Text offsets 独立约束物理正文边界。
        match event {
            Event::Start(Tag::Table(_)) => table = true,
            Event::End(TagEnd::Table) => table = false,
            Event::Start(Tag::CodeBlock(_)) => code = true,
            Event::End(TagEnd::CodeBlock) => code = false,
            Event::Start(Tag::Heading { .. }) => atx = source.as_bytes()[range.start] == b'#',
            Event::End(TagEnd::Heading(_)) => atx = false,
            Event::Text(_) if !table && !code && !atx => ranges.push(range),
            _ => (), // HTML、标记与 break 不属于这里的普通正文 oracle。
        }
    }
    ranges
}

fn assert_line_oracle(
    source: &str,
    lines: &[&VerifiedMarkdownOrdinaryLine],
    expected: &[Range<usize>],
) {
    assert_eq!(lines.len(), expected.len());
    for (actual, content) in lines.iter().zip(expected) {
        // 独立按原字节找真实 CR/LF，不使用新 origin 输出生成期望值。
        let physical = source.as_bytes()[..content.start]
            .iter()
            .rposition(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(0, |byte| byte + 1);
        let content_end = source.as_bytes()[content.start..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(source.len(), |offset| content.start + offset);
        assert_eq!(
            content.end, content_end,
            "本 oracle 夹具没有行尾空白或 inline markup"
        );
        let eol_end = match source.as_bytes().get(content_end..) {
            Some([b'\r', b'\n', ..]) => content_end + 2,
            Some([b'\r' | b'\n', ..]) => content_end + 1,
            _ => content_end,
        };
        assert_eq!(actual.origin().physical_range(), physical..eol_end);
        assert_eq!(actual.origin().prefix_range(), physical..content.start);
        assert_eq!(actual.origin().content_range(), content.clone());
        assert_eq!(actual.origin().eol_range(), content_end..eol_end);
        assert_eq!(
            actual.physical_utf16_range(),
            units(source, physical)..units(source, eol_end)
        );
        assert_eq!(
            actual.prefix_utf16_range(),
            units(source, physical)..units(source, content.start)
        );
        assert_eq!(
            actual.content_utf16_range(),
            units(source, content.start)..units(source, content_end)
        );
        assert_eq!(
            actual.eol_utf16_range(),
            units(source, content_end)..units(source, eol_end)
        );
    }
}

#[test]
fn mixed_real_ordinary_table_and_leaf_sources_match_full_parser_and_physical_bytes() {
    let source = "前段😀\r\n续行\r\n\r\n> 引用一\r\n> 引用二\r\n\r\n- [x] 任务一\n  任务二\n\n| H | C |\n| - | - |\n| a | b |\n\n# 原生标题\n\n```rust\nlet value = 1;\n```\n\nSetext 中\r\n===\r\n\r\n末段";
    let (_directory, doc) = document(source);
    let owners = Arc::strong_count(doc.root.as_ref().unwrap());
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let lines = sources
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert_line_oracle(source, &lines, &ordinary_text_ranges(source));
    let leaves = sources
        .ordinary_leaves_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert!(
        leaves
            .iter()
            .any(|leaf| matches!(leaf.kind(), LeafBlockKind::AtxHeading(_)))
    );
    assert!(
        leaves
            .iter()
            .any(|leaf| leaf.kind() == LeafBlockKind::FencedCode)
    );
    assert_eq!(
        sources
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .windows_for(&doc)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(Arc::strong_count(doc.root.as_ref().unwrap()), owners);
    assert!(
        request.total_work().source_bytes > source.len(),
        "结构、来源和坐标真实重读均须入账"
    );
    assert_eq!(
        request.total_work().copied_source_bytes,
        sources
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .budget()
            .source_bytes,
        "ordinary 元数据不能另建正文 String；只有原有限 table 输入允许复制"
    );
}

#[test]
fn middle_of_long_paragraph_keeps_same_unfinished_origin_and_global_for_forward_extension() {
    let source = (0..1200)
        .map(|line| format!("第{line}行中😀\r\n"))
        .collect::<String>();
    let starts = source
        .match_indices('第')
        .map(|(byte, _)| byte)
        .collect::<Vec<_>>();
    let (_directory, doc) = document(&source);
    let viewport = units(&source, starts[400])..units(&source, starts[405]);
    let mut request = doc
        .viewport_source_request(viewport.clone(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let old = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let global = request.batch.source_probe_state().0;
    let cursor = match request.job.as_ref().unwrap() {
        OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
        OrdinaryJob::Leaf(_) => panic!("该真实夹具只有一个 paragraph"),
    };
    assert_eq!(request.origin_forward_floor(), viewport.end);
    assert!(
        request.batch.forward_resume_floor() > request.origin_forward_floor(),
        "分类水位不能代替已消费来源"
    );
    let old_work = request.total_work();
    let next = units(&source, starts[403])..units(&source, starts[411]);
    request
        .continue_forward(&doc, &old, next.clone(), authority(&doc))
        .unwrap();
    assert_eq!(request.batch.source_probe_state().0, global);
    assert_eq!(
        match request.job.as_ref().unwrap() {
            OrdinaryJob::Inline(job) => std::ptr::from_ref(job.cursor.as_ref()).addr(),
            _ => 0,
        },
        cursor
    );
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let expected = ordinary_text_ranges(&source)
        .into_iter()
        .filter(|range| {
            let start = units(&source, range.start);
            next.start <= start && start < next.end
        })
        .collect::<Vec<_>>();
    let lines = sources
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert_line_oracle(&source, &lines, &expected);
    assert_eq!(request.batch.source_probe_state().0, global);
    assert!(request.total_work().source_bytes > old_work.source_bytes);
    assert!(
        request.total_work().source_bytes - old_work.source_bytes < source.len() / 4,
        "同一未完 cursor 前进不能重扫原 Global 或已处理的段前缀"
    );
    assert!(
        old.ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .next()
            .is_some(),
        "旧 immutable 来源仍可读"
    );
    assert_eq!(
        request
            .continue_forward(&doc, &old, next.clone(), authority(&doc))
            .unwrap_err(),
        MarkdownTableRequestError::PublicationSuperseded
    );
    assert_eq!(
        request
            .continue_forward(&doc, &sources, viewport, authority(&doc))
            .unwrap_err(),
        MarkdownTableRequestError::CoverageRequiresCheckpoint
    );
}

#[test]
fn continuous_coordinates_visit_short_line_bytes_once_across_paragraph_boundaries() {
    let source = "中😀\r\n\r\n短\n".repeat(80);
    let (_directory, doc) = document(&source);
    let mut boundaries = Vec::new();
    let mut start = 0;
    while start < source.len() {
        let end = source.as_bytes()[start..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(source.len(), |length| start + length);
        let eol_end = if source.as_bytes().get(end..end + 2) == Some(b"\r\n") {
            end + 2
        } else {
            (end + 1).min(source.len())
        };
        boundaries.push([start, start, end, eol_end]);
        start = eol_end;
    }
    let (mut coordinates, initial) = Coordinates::new(&doc, boundaries[0], 4096).unwrap();
    assert!(initial.source_tree_steps > 0 && initial.source_bytes == 0);
    let mut bytes = 0;
    for (index, boundary) in boundaries.into_iter().enumerate() {
        if index != 0 {
            let reset = coordinates.continue_at(boundary).unwrap();
            assert_eq!(reset.source_tree_steps, 0, "续行不能重新定位相同 leaf");
            assert_eq!(reset.source_bytes, 0);
        }
        loop {
            let (work, values) = coordinates.advance(&doc, 4096).unwrap();
            bytes += work.source_bytes;
            if let Some(values) = values {
                assert_eq!(values, boundary.map(|byte| units(&source, byte)));
                break;
            }
        }
    }
    assert_eq!(bytes, source.len(), "不能逐条 origin 从同 leaf 起点重扫");
}

#[test]
fn giant_unicode_line_has_one_origin_record_and_bounded_real_coordinate_steps() {
    let source = "中😀e\u{301}".repeat(10000);
    let (_directory, doc) = document(&source);
    let viewport = 10000..10100;
    let mut request = doc
        .viewport_source_request(viewport, 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let lines = sources
        .ordinary_lines_for(&doc, authority(&doc))
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].origin().content_range(), 0..source.len());
    assert_eq!(lines[0].content_utf16_range(), 0..doc.utf16_len());
    assert_eq!(request.total_work().copied_source_bytes, 0);
    assert_eq!(request.total_work().allocated_source_capacity, 0);
    // 独立桥接预算取多个不同切点，迫使 frame 可在 scalar 中部结束，读取总量仍恰为真实源长。
    for budget in [2048, 2051, 4096] {
        let (mut coordinates, _) =
            Coordinates::new(&doc, [0, 0, source.len(), source.len()], budget).unwrap();
        let mut bytes = 0;
        loop {
            let (work, values) = coordinates.advance(&doc, budget).unwrap();
            bytes += work.source_bytes;
            assert!(work.source_bytes <= budget && work.structural_transitions <= budget);
            if let Some(values) = values {
                assert_eq!(values, [0, 0, doc.utf16_len(), doc.utf16_len()]);
                break;
            }
        }
        assert_eq!(bytes, source.len());
    }
}

#[test]
fn repeated_real_source_publications_exhaust_original_total_fuel_without_reset() {
    let source = "one\ntwo\n";
    let (_directory, doc) = document(source);
    let viewport = 0..doc.utf16_len();
    let mut request = doc
        .viewport_source_request(viewport.clone(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let initial = request.total_work();
    let mut exhausted = false;
    for _ in 0..10000 {
        let continued =
            request.continue_forward(&doc, &previous, viewport.clone(), authority(&doc));
        if continued == Err(MarkdownTableRequestError::WorkLimit) {
            exhausted = true;
            break;
        }
        continued.unwrap();
        match finish(&mut request, &doc) {
            Err(MarkdownTableRequestError::WorkLimit) => {
                exhausted = true;
                break;
            }
            Ok(()) => {
                previous = request
                    .take_sources_for(&doc, authority(&doc))
                    .unwrap()
                    .unwrap()
            }
            Err(error) => panic!("实际有限发布出现其它失败 {error:?}"),
        }
    }
    assert!(
        exhausted,
        "新 publication 不能重新取得原请求的总 transitions fuel"
    );
    assert!(request.total_work().structural_transitions > initial.structural_transitions);
    assert_eq!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::WorkLimit
    );
}

#[test]
fn context_and_full_reverse_selection_are_checked_and_mutating_failure_is_sticky() {
    let source = "first\nsecond\n\nthird\n";
    let (_directory, mut doc) = document(source);
    let selection = MarkdownTextSelection {
        anchor_utf16: 1,
        head_utf16: 4,
    };
    doc.set_selection(selection).unwrap();
    for context_change in [false, true] {
        let mut request = doc
            .viewport_source_request(0..doc.utf16_len(), 41, selection)
            .unwrap();
        let mut wrong = authority(&doc);
        if context_change {
            wrong.context_revision += 1;
        } else {
            wrong.selection = MarkdownTextSelection {
                anchor_utf16: 4,
                head_utf16: 1,
            };
            doc.set_selection(wrong.selection).unwrap();
        }
        assert_eq!(
            request.advance(&doc, wrong, 4096).unwrap_err(),
            MarkdownTableRequestError::PublicationSuperseded
        );
        doc.set_selection(selection).unwrap();
        assert_eq!(
            request.advance(&doc, authority(&doc), 4096).unwrap_err(),
            MarkdownTableRequestError::PublicationSuperseded
        );
        assert!(request.job.is_none() && request.coordinates.is_none());
    }
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, selection)
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    let mut wrong = authority(&doc);
    wrong.context_revision += 1;
    assert!(matches!(
        sources.ordinary_lines_for(&doc, wrong),
        Err(MarkdownTableRequestError::PublicationSuperseded)
    ));
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        request
            .continue_forward(&doc, &sources, 0..doc.utf16_len(), cancelled)
            .unwrap_err(),
        MarkdownTableRequestError::Cancelled
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::Cancelled
    );
    assert!(request.job.is_none() && request.coordinates.is_none());
    assert!(
        sources
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .next()
            .is_some()
    );
}

#[test]
fn cancelled_take_releases_pending_payload_and_cannot_be_revived_by_correct_authority() {
    let source = "first\nsecond\nthird\n";
    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let mut cancelled = authority(&doc);
    cancelled.cancelled = true;
    assert_eq!(
        request.take_sources_for(&doc, cancelled).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert!(request.ready.is_none() && request.job.is_none() && request.coordinates.is_none());
    assert_eq!(
        request.take_sources_for(&doc, authority(&doc)).err(),
        Some(MarkdownTableRequestError::Cancelled)
    );
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::Cancelled
    );
}

#[test]
fn edit_undo_same_root_new_generation_and_other_document_cannot_reuse_sources() {
    let source = "中文😀\nsecond\n\nthird";
    let (_directory, mut doc) = document(source);
    let (_other_directory, other) = document(source);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert!(matches!(
        sources.ordinary_lines_for(&other, authority(&other)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    let root = Arc::downgrade(doc.root.as_ref().unwrap());
    let generation = doc.generation();
    doc.insert_utf16(0, "插入", MarkdownTextSelection::caret(2))
        .unwrap();
    doc.undo().unwrap();
    assert!(std::sync::Weak::ptr_eq(
        &root,
        &Arc::downgrade(doc.root.as_ref().unwrap())
    ));
    assert!(doc.generation() > generation);
    assert!(matches!(
        sources.ordinary_lines_for(&doc, authority(&doc)),
        Err(MarkdownTableRequestError::SourceSuperseded)
    ));
    assert_eq!(
        request.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::SourceSuperseded
    );
    assert!(request.job.is_none() && request.coordinates.is_none());
}

#[test]
fn publication_is_a_separate_counted_step_and_coordinate_setup_is_counted_before_cancel() {
    let source = "first\nsecond\nthird\n";
    let (_directory, doc) = document(source);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    let mut saw_coordinates = false;
    for _ in 0..10000 {
        let before = request.total_work();
        request.advance(&doc, authority(&doc), 4096).unwrap();
        if request.coordinates.is_some() && request.coordinate_active {
            let after = request.total_work();
            assert!(
                after.source_tree_steps > before.source_tree_steps,
                "首次 Coordinates::new 的真实树定位必须当步归账"
            );
            request.cancel();
            assert!(request.coordinates.is_none() && request.job.is_none());
            saw_coordinates = true;
            break;
        }
    }
    assert!(saw_coordinates);
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    for _ in 0..10000 {
        request.advance(&doc, authority(&doc), 4096).unwrap();
        if request.batch.ready_for(&doc).unwrap().is_some() {
            assert!(
                request.ready.is_none(),
                "batch Ready 的尾部不免费创建来源 publication"
            );
            let before = request.total_work();
            let step = request.advance(&doc, authority(&doc), 4096).unwrap();
            let MarkdownViewportSourceStep::SourcesAvailable { work } = step else {
                panic!("独立发布步应完成")
            };
            assert!(work.structural_transitions >= 2 * CONTROL_COST);
            assert!(work.metadata_bytes >= publication_metadata());
            assert_eq!(
                request.total_work().structural_transitions - before.structural_transitions,
                work.structural_transitions
            );
            assert_eq!(
                request.total_work().metadata_bytes - before.metadata_bytes,
                work.metadata_bytes
            );
            return;
        }
    }
    panic!("未经过显式来源发布边界");
}

#[test]
fn source_records_are_bounded_without_losing_the_original_immutable_publication() {
    let source = (0..300)
        .map(|line| format!("x{line}\n"))
        .collect::<String>();
    let starts = source
        .match_indices('x')
        .map(|(byte, _)| byte)
        .collect::<Vec<_>>();
    let (_directory, doc) = document(&source);
    let mut request = doc
        .viewport_source_request(0..starts[100], 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    request
        .continue_forward(&doc, &previous, starts[90]..starts[200], authority(&doc))
        .unwrap();
    let metadata_before = request.batch.source_probe_state().2;
    assert!(
        metadata_before
            >= previous.retained_metadata + source_workspace_metadata() + publication_metadata(),
        "旧来源记录仍活时先联合预扣，不能修剪后才退款"
    );
    finish(&mut request, &doc).unwrap();
    let next = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert_eq!(
        next.ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        110
    );
    assert!(request.batch.source_probe_state().2 >= metadata_before + 100 * record_metadata());
    assert_eq!(
        previous
            .ordinary_lines_for(&doc, authority(&doc))
            .unwrap()
            .count(),
        100
    );
    // 单页超限返回 typed 失败；不能以 Source fallback 或部分 ordinary Ready 掩盖缺行。
    let mut excessive = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    assert_eq!(
        finish(&mut excessive, &doc).unwrap_err(),
        MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit)
    );
    assert!(
        excessive.records.is_empty() && excessive.job.is_none() && excessive.coordinates.is_none()
    );
    assert_eq!(
        excessive.advance(&doc, authority(&doc), 4096).unwrap_err(),
        MarkdownTableRequestError::Window(MarkdownTableWindowError::SemanticLimit)
    );
}

fn table_source(rows: usize, columns: usize) -> String {
    let mut source = String::new();
    source.push_str(&format!("|{}\n", " H |".repeat(columns)));
    source.push_str(&format!("|{}\n", " - |".repeat(columns)));
    for _ in 0..rows {
        source.push_str(&format!("|{}\n", " x |".repeat(columns)));
    }
    source
}

#[test]
fn forward_jump_still_admits_live_previous_table_inventory_before_new_table() {
    let old = table_source(60, 16);
    let later = table_source(4, 16);
    let source = format!("{old}\n间隔\n\n{later}");
    let later_start = source.len() - later.len();
    let (_directory, doc) = document(&source);
    let mut request = doc
        .viewport_source_request(0..old.len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let previous = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    assert!(previous.table.budget().cells >= 960);
    assert!(units(&source, later_start) > request.batch.forward_resume_floor());
    let viewport = units(&source, later_start)..doc.utf16_len();
    request
        .continue_forward(&doc, &previous, viewport.clone(), authority(&doc))
        .unwrap();
    assert_eq!(
        finish(&mut request, &doc).unwrap_err(),
        MarkdownTableRequestError::Window(MarkdownTableWindowError::CellLimit)
    );
    assert_eq!(
        previous
            .table_sources_for(&doc, authority(&doc))
            .unwrap()
            .windows_for(&doc)
            .unwrap()
            .len(),
        1
    );
    // 新表独立确实合法；拒绝由旧大表与新表同活的联合库存触发，而非测试输入本身损坏。
    let mut independent = doc
        .viewport_source_request(viewport, 41, doc.selection())
        .unwrap();
    finish(&mut independent, &doc).unwrap();
    assert_eq!(
        independent
            .sources_for(&doc, authority(&doc))
            .unwrap()
            .unwrap()
            .table
            .windows_for(&doc)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn request_and_sources_never_keep_document_root_alive_after_close_owner_drop() {
    let source = "> first😀\r\n> second\r\n\r\n# leaf\r\n";
    let (_directory, doc) = document(source);
    let root = Arc::downgrade(doc.root.as_ref().unwrap());
    let mut request = doc
        .viewport_source_request(0..doc.utf16_len(), 41, doc.selection())
        .unwrap();
    finish(&mut request, &doc).unwrap();
    let sources = request
        .take_sources_for(&doc, authority(&doc))
        .unwrap()
        .unwrap();
    drop(doc);
    assert_eq!(
        root.strong_count(),
        0,
        "来源及 shared scope 页不能强持 piece root/mmap"
    );
    assert!(!sources.records.is_empty());
    request.cancel();
    assert!(request.records.is_empty() && request.coordinates.is_none() && request.job.is_none());
    drop(sources);
}
