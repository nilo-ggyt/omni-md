//! 完整Parser只作合成小输入oracle；产品adapter只接当前行和独立真实上下文。

use super::*;

/// 测试fixture可遍历全文建立真实坐标，不作为产品context producer。
fn lines(
    source: &str,
    header_line: usize,
    prefix_bytes: usize,
) -> Vec<MarkdownTableSourceLine<'_>> {
    let mut byte_start = 0;
    let mut utf16_start = 0;
    source
        .split_inclusive('\n')
        .enumerate()
        .map(|(source_line, text)| {
            let line = MarkdownTableSourceLine {
                text,
                byte_start,
                utf16_start,
                source_line,
                row_ordinal: source_line.saturating_sub(header_line + 1),
                container_prefix_bytes: prefix_bytes,
            };
            byte_start += text.len();
            utf16_start += text.encode_utf16().count();
            line
        })
        .collect()
}

/// 默认无外部引用/underline的fixture明确提供Ready，不用空值代替未知状态。
fn input<'a>(
    all: &'a [MarkdownTableSourceLine<'a>],
    body: Range<usize>,
) -> MarkdownTableWindowInput<'a> {
    MarkdownTableWindowInput {
        generation: 7,
        context_revision: 11,
        header: all[0],
        delimiter: all[1],
        body: &all[body],
        include_header: false,
        inherited_styles: MarkdownPresentationStyles::default(),
        container_prefix_runs: &[],
        global: MarkdownTableGlobalContext::Ready {
            reference_definitions: "",
            underline: &[],
        },
        active_selection: usize::MAX..usize::MAX,
    }
}

/// 对已经验证过的不相交runs作二分查询，不让测试oracle自身逐字符平方扫描。
fn styles_at(runs: &[MarkdownPresentationRun], at: usize) -> MarkdownPresentationStyles {
    let position = runs.partition_point(|run| run.utf16_range.end <= at);
    runs.get(position)
        .filter(|run| run.utf16_range.contains(&at))
        .map_or(MarkdownPresentationStyles::default(), |run| run.styles)
}

/// 在当前完整源行逐UTF16比较，包括中文、surrogate的两个unit与真实EOL。
fn compare_with_full_source(
    source: &str,
    input: &MarkdownTableWindowInput<'_>,
) -> MarkdownProjectedTableWindow {
    let oracle = build_markdown_presentation(source, 0, &input.active_selection).unwrap();
    let window = project_table_window(input).unwrap();
    for origin in window.origins.iter().filter(|origin| !origin.context_only) {
        for at in origin.source_utf16.clone() {
            assert_eq!(
                styles_at(&window.runs, at),
                styles_at(&oracle.runs, at),
                "UTF16={at}"
            );
        }
    }
    for row in &window.rows {
        let actual = &window.cells[row.cell_index_range.clone()];
        let expected = oracle
            .tables
            .iter()
            .flat_map(|table| table.rows.iter().map(move |row| (table, row)))
            .find(|(_, candidate)| candidate.utf16_range.end == row.utf16_range.end)
            .unwrap();
        assert_eq!(actual.len(), expected.1.cell_index_range.len());
        for (actual, expected) in actual
            .iter()
            .zip(&expected.0.cells[expected.1.cell_index_range.clone()])
        {
            match &actual.source {
                MarkdownTableWindowCellSource::Present(range) => {
                    assert_eq!(*range, expected.utf16_range)
                }
                MarkdownTableWindowCellSource::ImplicitEmpty {
                    insertion_anchor_utf16,
                } => {
                    assert!(expected.utf16_range.is_empty()); // parser补格的anchor可在EOL后，输出只留行内容尾。
                    assert_eq!(*insertion_anchor_utf16, row.utf16_range.end);
                }
            }
            assert_eq!(actual.column_index, expected.column_index);
            assert_eq!(actual.alignment, expected.alignment);
        }
    }
    window
}

#[test]
fn middle_rows_of_large_table_keep_absolute_ordinal_and_bounded_projection() {
    // 同一真实语义fixture超过旧128Ki materializer预算，但本次只投影3个完整行。
    let header = "| H1 | H2 |\n| --- | --- |\n";
    let row = format!("| {} | y |\n", "x".repeat(240));
    let source = format!("{header}{}", row.repeat(5_000));
    let all = lines(&source, 0, 0);
    let request = input(&all, 4_502..4_505);
    let window = project_table_window(&request).unwrap();
    assert_eq!(window.column_count, 2);
    assert!(window.row(0).is_none());
    assert!(window.row(4_500).is_none());
    assert_eq!(window.row(4_501).unwrap(), &window.rows[0]);
    assert_eq!(window.cell(4_501, 1).unwrap().column_index, 1);
    assert!(window.cell(4_501, 2).is_none());
    assert!(window.cell(4_504, 0).is_none());
    // 旧presentation自身有8192装饰上限，不可作为5000行完整oracle；测试直接消费固定Parser真实事件。
    assert!(source.is_ascii());
    let first_byte = all[4_502].byte_start;
    let last_byte = all[4_504].byte_start + all[4_504].text.len();
    let mut actual_ordinal = 0;
    let mut oracle_rows = Vec::new();
    let mut oracle_cells = Vec::new();
    for (event, range) in Parser::new_ext(&source, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(Tag::TableRow) => {
                actual_ordinal += 1;
                if range.start >= first_byte && range.end <= last_byte {
                    oracle_rows.push((
                        actual_ordinal,
                        markdown_table_line_content_range(&source, range).unwrap(),
                    )); // 只收当前3行metadata。
                }
            }
            Event::Start(Tag::TableCell) if range.start >= first_byte && range.end <= last_byte => {
                oracle_cells.push(trim_markdown_table_cell_range(&source, range).unwrap()); // 全量Parser仅在test存在，绝不接产品。
            }
            _ => {} // 其余事件不保留，不把test oracle建立成第二份结果AST。
        }
    }
    assert_eq!(oracle_rows.len(), 3);
    for (actual, (ordinal, bytes)) in window.rows.iter().zip(oracle_rows) {
        assert_eq!(actual.row_ordinal, ordinal);
        assert_eq!(actual.byte_range, bytes);
    }
    for (actual, range) in window.cells.iter().zip(oracle_cells) {
        assert_eq!(actual.source, MarkdownTableWindowCellSource::Present(range)); // 本fixture纯ASCII，byte与UTF16端点相同。
    }
    assert_eq!(window.rows.len(), 3);
    assert_eq!(window.cells.len(), 6);
    assert_eq!(window.rows[0].row_ordinal, 4_501);
    assert_eq!(window.rows[0].source_line, 4_502);
    assert_eq!(window.projection_bytes, header.len() + row.len() * 3);
    assert!(window.projection_bytes < 1_024);
    assert_eq!(window.origins.len(), 5);
    assert!(window.origins[..2].iter().all(|origin| origin.context_only));
    assert!(
        window
            .runs
            .iter()
            .all(|run| run.utf16_range.start >= all[4_502].utf16_start)
    );
}

#[test]
fn table_above_old_total_row_limit_keeps_only_requested_row_semantics() {
    // 300行仍可由既有presentation完整作oracle；新结果不因全表超过256行而拒绝预览。
    let source = format!("| H1 | H2 |\n| --- | --- |\n{}", "| a | b |\n".repeat(300));
    let all = lines(&source, 0, 0);
    let request = input(&all, 152..153);
    let window = compare_with_full_source(&source, &request);
    assert_eq!(window.rows.len(), 1);
    assert_eq!(window.rows[0].row_ordinal, 151);
    assert_eq!(window.projection_bytes, 36);
}

#[test]
fn real_gfm_rows_preserve_escaped_pipes_ragged_cells_and_inline_block_lookalikes() {
    let source = "a | b | c\n:-- | :-: | --:\nplain\n| # h | - x | > q | ignored |\n| `a\\|b` | **中😀e\u{301}** | ~~gone~~ |\n| a\\\\ | _b_ | $x$ |\n";
    let all = lines(source, 0, 0);
    let request = input(&all, 2..all.len());
    let window = compare_with_full_source(source, &request);
    assert_eq!(window.rows.len(), 4);
    assert!(window.cells.iter().any(|cell| matches!(
        cell.source,
        MarkdownTableWindowCellSource::ImplicitEmpty { .. }
    )));
    assert!(window.cells.iter().all(|cell| cell.column_index < 3));
}

#[test]
fn unicode_crlf_and_source_gap_use_independent_byte_and_utf16_origins() {
    let source = "| 中文 | 😀 |\r\n| - | - |\r\n| old | old |\r\n| **😀** | e\u{301} |\r\n| tail | end |\r\n";
    let all = lines(source, 0, 0);
    let mut request = input(&all, 3..5);
    request.active_selection = all[3].utf16_start + 3..all[3].utf16_start + 5;
    let window = compare_with_full_source(source, &request);
    assert_eq!(window.rows[0].row_ordinal, 2);
    assert_ne!(
        window.origins[2].source_bytes.start,
        window.origins[2].source_utf16.start
    );
    assert_eq!(window.origins[2].source_bytes.start, all[3].byte_start);
    assert_eq!(window.origins[2].source_utf16.start, all[3].utf16_start);
    assert!(window.origins[2].source_bytes.start > window.origins[1].source_bytes.end);
}

#[test]
fn external_reference_uses_parser_label_rules_and_footnotes_remain_literal() {
    let definitions = "[å]: https://example.test/first \"first\"\n[Å]: https://example.test/second\n[^note]: footnote body\n";
    let source = format!("| H |\n| - |\n| [Å] [^note] |\n\n{definitions}");
    let all = lines(&source, 0, 0);
    let mut request = input(&all, 2..3);
    request.global = MarkdownTableGlobalContext::Ready {
        reference_definitions: definitions,
        underline: &[],
    };
    let window = compare_with_full_source(&source, &request);
    assert!(
        window
            .runs
            .iter()
            .any(|run| run.styles.contains(MarkdownPresentationStyles::LINK))
    );
}

#[test]
fn omitted_rows_do_not_change_literal_dollars_or_ordinary_inline_styles() {
    // 美元和花括号属于普通UTF8内容；省略前行不应再改变后行的字重、代码或链接语义。
    let hidden = format!("$x${}", "{}".repeat(255));
    let source = format!(
        "| H |\n| - |\n| {hidden} |\n| $}}x{{y$ **加粗** `代码` [链接](https://example.test) |\n"
    );
    let all = lines(&source, 0, 0);
    let request = input(&all, 3..4);
    let window = compare_with_full_source(&source, &request);
    for (byte, _) in source
        .match_indices('$')
        .filter(|(at, _)| *at >= all[3].byte_start)
    {
        let at = source[..byte].encode_utf16().count();
        assert!(!styles_at(&window.runs, at).contains(MarkdownPresentationStyles::MARKER));
        assert!(!styles_at(&window.runs, at).contains(MarkdownPresentationStyles::HIDDEN_MARKER));
    }
    #[cfg(feature = "s2-table-window-prototype")]
    {
        // 同一普通Parser也收集链接引用，美元文字不再吞掉后继真实链接。
        assert!(table_window_reference_queries(&request).unwrap().is_ready());
    }
}

#[cfg(feature = "s2-table-window-prototype")]
#[test]
fn finite_query_values_feed_the_same_window_styles_without_definition_text_retention() {
    // 真实源上的完整Parser只作测试oracle；窗口本身只持当前行和实际用到的两个有限值。
    let definitions = "[Straße]: /first \"标题\"\n[STRASSE]: /later\n[图]: /image\n";
    let source = format!(
        "| H1 | H2 |\n| --- | --- |\n| skipped | row |\n| [中文][STRASSE] | ![图] [missing] |\n\n{definitions}"
    );
    let all = lines(&source, 0, 0);
    let mut request = input(&all, 3..4);
    let mut queries = table_window_reference_queries(&request).unwrap();
    let parser = Parser::new_ext(definitions, markdown_presentation_options(false));
    for label in ["Straße", "STRASSE", "图"] {
        // 源先后顺序是producer义务；duplicate命中在有限槽内保持first-wins而不覆盖旧值。
        let value = parser.reference_definitions().get(label).unwrap();
        queries
            .admit_verified_value(label, &value.dest, value.title.as_deref().unwrap_or(""))
            .unwrap();
    }
    {
        // Pending与Ready是不同借用阶段；源解析完成前不会持有可用于原生发布的旧resolver借用。
        let mut pending_request = input(&all, 3..4);
        pending_request.global = MarkdownTableGlobalContext::ReadyResolved {
            references: &queries,
            underline: &[],
        };
        assert_eq!(
            project_table_window(&pending_request),
            Err(MarkdownTableWindowError::ContextPending)
        );
    }
    queries.finish_verified_eof();
    request.global = MarkdownTableGlobalContext::ReadyResolved {
        references: &queries,
        underline: &[],
    };
    let resolved = compare_with_full_source(&source, &request);
    request.global = MarkdownTableGlobalContext::Ready {
        reference_definitions: definitions,
        underline: &[],
    };
    assert_eq!(resolved, project_table_window(&request).unwrap());
}

#[test]
fn source_underline_crossing_gap_is_not_repaired_by_projected_tag_pairing() {
    let source =
        "| <u>H |\n| - |\n| hidden </u> |\n| visible </u> |\n| <u>also |\n| final </u> |\n";
    let all = lines(source, 0, 0);
    let mut request = input(&all, 3..all.len());
    let first_open = source.find("<u>").unwrap();
    let first_close = source.find("</u>").unwrap();
    let second_open = source[first_close + 4..].find("<u>").unwrap() + first_close + 4;
    let second_close = source[second_open..].find("</u>").unwrap() + second_open;
    let structures = [
        MarkdownTableUnderline {
            open: first_open..first_open + 3,
            content: first_open + 3..first_close,
            close: first_close..first_close + 4,
        },
        MarkdownTableUnderline {
            open: second_open..second_open + 3,
            content: second_open + 3..second_close,
            close: second_close..second_close + 4,
        },
    ];
    request.global = MarkdownTableGlobalContext::Ready {
        reference_definitions: "",
        underline: &structures,
    };
    let window = compare_with_full_source(source, &request);
    assert!(
        !styles_at(&window.runs, all[3].utf16_start + 3)
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );
    assert!(
        styles_at(&window.runs, all[4].utf16_start + 6)
            .contains(MarkdownPresentationStyles::UNDERLINE)
    );
}

#[test]
fn container_prefix_is_explicit_context_rather_than_trimmed_cell_content() {
    let source = "> | H |\n> | - |\n> | old |\n> | **中😀** |\n> | tail |\n";
    let all = lines(source, 0, 2);
    let oracle = build_markdown_presentation(source, 0, &(usize::MAX..usize::MAX)).unwrap();
    let mut prefixes = Vec::new();
    for line in &all[3..] {
        for run in &oracle.runs {
            let clipped = run.utf16_range.start.max(line.utf16_start)
                ..run.utf16_range.end.min(line.utf16_start + 2);
            if clipped.start < clipped.end {
                prefixes.push(MarkdownPresentationRun {
                    utf16_range: clipped,
                    styles: run.styles,
                }); // oracle提供真实prefix，不从裸投影发明容器语法。
            }
        }
    }
    let mut request = input(&all, 3..all.len());
    request.inherited_styles = MarkdownPresentationStyles::BLOCK_QUOTE;
    request.container_prefix_runs = &prefixes;
    let window = compare_with_full_source(source, &request);
    assert_eq!(window.rows[0].utf16_range.start, all[3].utf16_start);
    assert_eq!(window.origins[2].source_utf16.start, all[3].utf16_start + 2);
}

#[test]
fn source_budget_and_unknown_context_reject_before_partial_semantic_publication() {
    let source = "| H |\n| - |\n| tiny |\n";
    let all = lines(source, 0, 0);
    let mut request = input(&all, 2..3);
    request.global = MarkdownTableGlobalContext::Pending;
    assert_eq!(
        project_table_window(&request).unwrap_err(),
        MarkdownTableWindowError::ContextPending
    );
    request.global = MarkdownTableGlobalContext::Ready {
        reference_definitions: "",
        underline: &[],
    };
    let giant = "x".repeat(TABLE_WINDOW_MAX_BYTES + 1);
    let giant_row = [MarkdownTableSourceLine {
        text: &giant,
        ..all[2]
    }];
    request.body = &giant_row;
    assert_eq!(
        project_table_window(&request).unwrap_err(),
        MarkdownTableWindowError::SourceLimit
    );
}

#[test]
fn projection_success_does_not_prove_membership_across_an_omitted_blank() {
    // 这是producer必须另证成员的真实反例；adapter只返回明确的候选，不进入产品消费者。
    let source = "| H |\n| - |\n| member |\n\n| unrelated |\n";
    let all = lines(source, 0, 0);
    let request = input(&all, 4..5);
    assert!(project_table_window(&request).is_ok());
    let full = build_markdown_presentation(source, 0, &(usize::MAX..usize::MAX)).unwrap();
    assert_eq!(full.tables[0].rows.len(), 2);
    assert!(full.tables[0].utf16_range.end <= all[3].utf16_start);
}

#[test]
fn metadata_intersections_stop_at_shared_admission_before_multiplicative_allocation() {
    // 当前真实源很小；每个有限prefix元数据都跨全部行，旧实现会先展开8192×255项才拒绝。
    let source = format!("| H |\n| - |\n{}", "| x |\n".repeat(255));
    let all = lines(&source, 0, 0);
    let metadata = vec![
        MarkdownPresentationRun {
            utf16_range: 0..source.len(),
            styles: MarkdownPresentationStyles::BLOCK_QUOTE,
        };
        MAX_MARKDOWN_PRESENTATION_DECORATIONS
    ];
    let mut request = input(&all, 2..all.len());
    request.container_prefix_runs = &metadata;
    assert_eq!(
        project_table_window(&request).unwrap_err(),
        MarkdownTableWindowError::SemanticLimit
    );
    // 直接检查准入边界：第8193个非空run不会push，拒绝后的长度和已分配容量均有确定上限。
    let mut admitted = Vec::new();
    for run in &metadata {
        admit_window_run(
            &mut admitted,
            run.clone(),
            RunAdmission {
                maximum: MAX_MARKDOWN_PRESENTATION_DECORATIONS,
                quota: None,
                output: false,
            },
        )
        .unwrap();
    }
    assert_eq!(
        admit_window_run(
            &mut admitted,
            metadata[0].clone(),
            RunAdmission {
                maximum: MAX_MARKDOWN_PRESENTATION_DECORATIONS,
                quota: None,
                output: false
            }
        )
        .unwrap_err(),
        MarkdownTableWindowError::SemanticLimit
    );
    assert_eq!(admitted.len(), MAX_MARKDOWN_PRESENTATION_DECORATIONS);
    assert_eq!(admitted.capacity(), MAX_MARKDOWN_PRESENTATION_DECORATIONS);
}

#[test]
fn include_header_and_header_only_windows_match_full_source_styles_and_cells() {
    let source = "| **H** | 中😀 |\n| :-- | --: |\n| one | two |\n| next | last |\n";
    let all = lines(source, 0, 0);
    let mut request = input(&all, 2..all.len());
    request.include_header = true;
    request.active_selection = 3..3;
    let window = compare_with_full_source(source, &request);
    assert!(window.rows[0].is_header);
    assert_eq!(window.rows[0].row_ordinal, 0);
    assert!(window.origins.iter().all(|origin| !origin.context_only));
    // 同一header也可单独成为窗口：delimiter仅给语法上下文，不伪造body或借下表高度。
    request.body = &[];
    let header_only = compare_with_full_source(source, &request);
    assert_eq!(header_only.rows.len(), 1);
    assert_eq!(header_only.cells.len(), 2);
    assert_eq!(header_only.rows[0].row_ordinal, 0);
}

#[test]
fn final_line_without_eol_distinguishes_real_empty_cell_from_parser_missing_cell() {
    let source = "| A | B | C |\n| - | - | - |\n| a | | c |\nplain";
    let all = lines(source, 0, 0);
    let request = input(&all, 2..all.len());
    let window = compare_with_full_source(source, &request);
    let explicit = &window.cells[window.rows[0].cell_index_range.clone()][1];
    assert!(
        matches!(explicit.source, MarkdownTableWindowCellSource::Present(ref range) if range.is_empty())
    );
    let implicit = &window.cells[window.rows[1].cell_index_range.clone()][1];
    assert_eq!(
        implicit.source,
        MarkdownTableWindowCellSource::ImplicitEmpty {
            insertion_anchor_utf16: source.len(),
        }
    );
    assert_eq!(window.rows[1].utf16_range.end, source.len());
    assert_eq!(
        window.origins.last().unwrap().source_bytes.end,
        source.len()
    );
}

#[test]
fn selection_in_omitted_gap_or_context_header_never_activates_visible_row_markers() {
    let source = "| **H** |\n| - |\n| **old** |\n| **visible** |\n";
    let all = lines(source, 0, 0);
    let mut request = input(&all, 3..4);
    // 离屏header虽参与语法解析，选择它的marker不能授予当前body活动态。
    request.active_selection = 3..3;
    let header_selection = compare_with_full_source(source, &request);
    // gap中的caret没有投影owner，不能因压缩后的坐标恰好落到visible行而激活它。
    request.active_selection = all[2].utf16_start + 3..all[2].utf16_start + 3;
    let gap_selection = compare_with_full_source(source, &request);
    assert_eq!(header_selection.runs, gap_selection.runs);
    assert!(gap_selection.runs.iter().any(|run| {
        run.styles
            .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            && run.utf16_range.start >= all[3].utf16_start
    }));
}
