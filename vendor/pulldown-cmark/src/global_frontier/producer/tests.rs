// Copyright 2015 Google Inc. All rights reserved.
// 原Parser事件和offset是独立语义oracle；完整MIT条款见LICENSE。
use super::*;
use crate::{Event, Parser, Tag};

fn dialect() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_MATH
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}
fn scanner(source: &str) -> GlobalFrontier {
    GlobalFrontier::new(
        19,
        source.len(),
        dialect(),
        GlobalFrontierLimits {
            spine: StructuralSpineLimits {
                max_depth: 16_384,
                max_live_page_bytes: 4 * 1024 * 1024,
                max_page_allocations: 1_000_000,
                max_role_work: 200_000_000,
            },
            max_byte_work: source.len().saturating_mul(1000) + 1_000_000,
            max_transitions: source.len().saturating_mul(10000) + 1_000_000,
        },
    )
    .unwrap()
}
fn consume(source: &str, width: usize) -> Vec<GlobalFact> {
    let mut producer = scanner(source);
    let mut facts = Vec::new();
    for _ in 0..source.len().saturating_mul(100) + 10000 {
        let start = producer.next_byte();
        let end = start.saturating_add(width).min(source.len());
        let step = producer
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                GlobalSourceAuthority {
                    current_revision: 19,
                    cancelled: false,
                },
                4096,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{error:?} source={source:?} anchor={start} committed={}",
                    producer.committed_byte()
                )
            });
        let work = match &step {
            GlobalStep::Progress { work, .. }
            | GlobalStep::NeedInput { work, .. }
            | GlobalStep::Fact { work, .. }
            | GlobalStep::Complete { work } => work,
        };
        assert!(
            work.byte_reads <= 4096 && work.transitions <= 4096,
            "真实step预算不能只统计输入准入"
        );
        match step {
            GlobalStep::Complete { .. } => return facts,
            GlobalStep::Fact { fact, .. } => facts.push(fact),
            _ => (),
        }
    }
    panic!("全局续体没有单调完成 source={source:?}");
}

#[test]
fn opening_quote_source_anchors_use_real_bytes_after_partial_tabs_at_every_frame_cut() {
    // 字面锚点先约束独立 Parser oracle；不能只让两个共享缩进算法互相证明同一错误。
    let cases: &[(&str, &[usize])] = &[
        ("> text\n", &[0]),
        ("> > text\n", &[0, 2]),
        (">\t> text\n", &[0, 2]),
        ("> \t> text\n", &[0, 3]),
        (" >\t> text\n", &[1, 3]),
        ("  >\t> text\r\n", &[2, 4]),
        ("   >\t> text\r\n", &[3, 5]),
        (">\t>\t> 中文😀\r\n", &[0, 2, 4]),
        ("> [!NOTE]\n> text\n", &[0]),
    ];
    for &(source, expected) in cases {
        let parser_anchors = Parser::new_ext(source, dialect())
            .into_offset_iter()
            .filter_map(|(event, range)| match event {
                Event::Start(Tag::BlockQuote(_)) => Some(range.start),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(parser_anchors, expected, "Parser source={source:?}");

        // 任意字节边界都可能中断 tab、CRLF 或多字节正文；只在真实源尾交付 EOF。
        for cut in 0..=source.len() {
            let actual = quote_anchors_at_frame_cut(source, cut, source.len());
            assert_eq!(actual, parser_anchors, "cut={cut} source={source:?}");
        }
        // 所有字节边界连续切分时，也不能被后续候选或 GFM 标签阶段覆盖尚未使用的锚点。
        assert_eq!(
            quote_anchors_at_frame_cut(source, source.len(), 1),
            parser_anchors,
            "width=1 source={source:?}"
        );
    }
}

fn quote_anchors_at_frame_cut(source: &str, cut: usize, width: usize) -> Vec<usize> {
    let mut producer = scanner(source);
    let authority = GlobalSourceAuthority {
        current_revision: 19,
        cancelled: false,
    };
    let mut anchors = None;
    for _ in 0..source.len().saturating_mul(100) + 10000 {
        let start = producer.next_byte();
        // 回看仍交付同一不可变源；cut 限制指定切点，width 另约束连续单字节交付。
        let end = (if start < cut { cut } else { source.len() }).min(start.saturating_add(width));
        let step = producer
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority,
                4096,
            )
            .unwrap();
        let work = match &step {
            GlobalStep::Progress { work, .. }
            | GlobalStep::NeedInput { work, .. }
            | GlobalStep::Fact { work, .. }
            | GlobalStep::Complete { work } => work,
        };
        assert!(work.byte_reads <= 4096 && work.transitions <= 4096);
        match step {
            GlobalStep::Fact {
                fact: GlobalFact::InlineBlock(block),
                ..
            } => {
                // 只读取真实已封闭段落的原 capture；每个夹具只有一个正文块，不重新扫描前缀。
                let private_anchors = (0..block.spine.len())
                    .filter_map(|depth| {
                        let entry = block.spine.entry(depth).unwrap();
                        (entry.kind == StructuralKind::Quote).then_some(entry.source_start)
                    })
                    .collect::<Vec<_>>();
                let mut cursor = block.scope_cursor().unwrap();
                let mut public_anchors = Vec::new();
                loop {
                    match cursor.advance(authority, 2048).unwrap() {
                        // 页构建可分步让出；导出的每个真实 Quote 层必须保持相同字节锚点。
                        CapturedScopeStep::Progress { .. } => (),
                        CapturedScopeStep::Entry {
                            role: CapturedScopeRole::Quote,
                            source_byte_start,
                            ..
                        } => public_anchors.push(source_byte_start),
                        CapturedScopeStep::Entry { .. } => (),
                        CapturedScopeStep::Complete { .. } => break,
                    }
                }
                assert_eq!(public_anchors, private_anchors);
                assert!(anchors.replace(private_anchors).is_none());
            }
            GlobalStep::Complete { .. } => {
                // 缺少正文块本身也是失败，不能把空结果当成未发现错位锚点。
                return anchors.expect("真实正文必须发布一个 InlineBlock capture");
            }
            // 非正文事实与续体让出不改变本夹具唯一的 scope 结果。
            _ => (),
        }
    }
    panic!("Quote 字节锚点夹具未有界完成 cut={cut} source={source:?}");
}

#[test]
fn mixed_real_block_frontiers_match_parser_without_prefix_eof_guessing() {
    let lines = [
        "text\n",
        "\n",
        "> text\n",
        ">\n",
        "- item\n",
        "1. item\n",
        "2. item\n",
        "  body\n",
        "    code\n",
        "|a|b|\n",
        "a|b\n",
        "|-|-|\n",
        "x|y\n",
        "```x\n",
        "```\n",
        "~~~\n",
        "<custom>\n",
        "</custom>\n",
        "<script>\n",
        "</script>\n",
        "[r]: /url\n",
        "[^f]: content\n",
        "---\n",
        "# heading\n",
        "> [!NOTE]\n",
        "- [x] task\n",
    ];
    let mut random = 0x1739_af02_2a1du64;
    for length in 0..4096 {
        let mut source = String::new();
        for _ in 0..length % 13 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            source.push_str(lines[random as usize % lines.len()]);
        }
        source.push_str("\n|end|table|\n|-|-|\nx|y\n");
        compare(&source, if length % 2 == 0 { 1 } else { 17 });
    }
}

#[test]
fn upstream_block_fixtures_keep_the_same_global_table_membership() {
    let mut count = 0;
    for suite in [
        include_str!("../../../tests/suite/spec.rs"),
        include_str!("../../../tests/suite/table.rs"),
        include_str!("../../../tests/suite/gfm_table.rs"),
        include_str!("../../../tests/suite/footnotes.rs"),
        include_str!("../../../tests/suite/blockquotes_tags.rs"),
        include_str!("../../../tests/suite/metadata_blocks.rs"),
        include_str!("../../../tests/suite/regression.rs"),
        include_str!("../../../tests/suite/math.rs"),
    ] {
        for item in suite.split("let original = r##\"").skip(1) {
            // 上游生成器用固定raw-string边界；只读取既有合成fixture，不解释项目或用户Markdown文件。
            let original = item.split("\"##;").next().unwrap();
            count += 1;
            let source = format!("{original}\n\n|after|table|\n|-|-|\nx|y\n");
            compare(&source, 17);
            compare_reference_definitions(&source);
        }
    }
    assert!(count >= 900);
}
fn compare_reference_definitions(source: &str) {
    use crate::block_context::scan_containers;
    use crate::scanners::LineStart;
    let parser = Parser::new_ext(source, dialect());
    let mut actual = std::collections::HashMap::new();
    for fact in consume(source, 17) {
        if let GlobalFact::Reference(reference) = fact {
            let candidate = *reference.candidate();
            let label = reference
                .normalized_label(
                    &source[candidate.label_start..candidate.label_end + 1],
                    |bytes| {
                        let mut line = LineStart::new(bytes);
                        scan_containers(&reference.spine, &mut line, reference.options);
                        line.scan_space(4);
                        Some(line.bytes_scanned())
                    },
                )
                .unwrap_or_else(|error| {
                    panic!("真实scope label必须与原Parser一致: {error:?} source={source:?}")
                });
            actual
                .entry(unicase::UniCase::new(label.into_string()))
                .or_insert(candidate.span_start..candidate.span_end);
        }
    }
    let expected = parser.reference_definitions();
    assert_eq!(
        actual.len(),
        expected.iter().count(),
        "refdef数量 source={source:?}"
    );
    for (label, definition) in expected.iter() {
        assert_eq!(
            actual.get(&unicase::UniCase::new(label.to_owned())),
            Some(&definition.span),
            "refdef span source={source:?}"
        );
    }
}
fn compare(source: &str, width: usize) {
    let expected = Parser::new_ext(source, dialect())
        .into_offset_iter()
        .filter_map(|(event, range)| {
            if let Event::Start(Tag::Table(align)) = event {
                Some((range.start, align.len()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let facts = consume(source, width);
    let actual = facts
        .iter()
        .filter_map(|fact| {
            if let GlobalFact::Table(table) = fact {
                Some((
                    table.header_byte_start + table.header_prefix_bytes,
                    table.columns,
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "width={width} source={source:?}");
    let expected_rows = Parser::new_ext(source, dialect())
        .into_offset_iter()
        .filter_map(|(event, range)| {
            if matches!(event, Event::Start(Tag::TableRow)) {
                Some(range.end)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let actual_rows = facts
        .iter()
        .filter_map(|fact| {
            if let GlobalFact::Row(row) = fact {
                Some(row.byte_end)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual_rows, expected_rows,
        "rows width={width} source={source:?}"
    );
    // 原Parser完整源Table范围独立决定真实尾；既有1002fixture与4096混合case同时覆盖新事实。
    let expected_ends = Parser::new_ext(source, dialect())
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, Event::End(crate::TagEnd::Table)).then_some(range.end)
        })
        .collect::<Vec<_>>();
    let actual_ends = facts
        .iter()
        .filter_map(|fact| {
            if let GlobalFact::TableEnd(end) = fact {
                assert_eq!(end.source_revision(), 19);
                assert_eq!(
                    end.utf16_end(),
                    source[..end.byte_end()].encode_utf16().count()
                );
                assert_eq!(
                    end.ended_with_eol(),
                    source
                        .as_bytes()
                        .get(end.byte_end().wrapping_sub(1))
                        .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
                );
                Some(end.byte_end())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual_ends, expected_ends,
        "table end width={width} source={source:?}"
    );
}

#[test]
fn real_global_containers_and_leaf_boundaries_match_parser_tables() {
    for source in [
        "| A | B |\n| - | - |\n| 中😀 | z |\n\n",
        "a | b\n--|--\nx|y\n",
        "paragraph\na|b\n-| -\nx|y\n",
        "paragraph\n|a|b|\n|-|-|\nx|y\n",
        "> | A | B |\n> | - | - |\n> | x | y |\n\n| C | D |\n|-|-|\nz\n",
        "- | A | B |\n  | - | - |\n  | x | y |\n- other\n\n|C|D|\n|-|-|\nz\n",
        "-\n\n  | A | B |\n  |-|-|\n  x|y\n",
        "> lazy\ncontinued\n\n|a|b|\n|-|-|\nx\n",
        "```x\n|fake|table|\n|-|-|\n```\n|a|b|\n|-|-|\nx|y\n",
        "<custom a='x'>\n|fake|table|\n|-|-|\n\n|a|b|\n|-|-|\nx|y\n",
        "<script>\n|fake|table|\n|-|-|\n</script>\n|a|b|\n|-|-|\nx|y\n",
        "---\nkey: value\n---\n|a|b|\n|-|-|\nx|y\n",
        "[^note]: |a|b|\n    |-|-|\n    x|y\n\n|C|D|\n|-|-|\nz\n",
        "- [x] |a|b|\n  |-|-|\n  x|y\n",
        "> [!NOTE]\n> |a|b|\n> |-|-|\n> x|y\n",
        "[r]: /url\n|a|b|\n|-|-|\nx|y\n",
        "a|b\\\n-|-\nx|y\n",
    ] {
        for width in [1, 3, 17, 4096] {
            compare(source, width);
        }
    }
}

#[test]
fn giant_ordinary_and_opaque_lines_do_not_hide_later_tables() {
    for prefix in [
        format!("{}\n\n", "中😀".repeat(200_000)),
        format!("```{}\nopaque\n```\n", "x".repeat(1024 * 1024)),
        format!("```\n{}\n```\n", "x".repeat(1024 * 1024)),
        format!("<custom attr='{}'>\nopaque\n\n", "x".repeat(1024 * 1024)),
        format!("<script>\n{}\n</script>\n", "x".repeat(1024 * 1024)),
    ] {
        let source = format!("{prefix}|a|b|\n|-|-|\nx|y\n");
        compare(&source, 8191);
    }
}

#[test]
fn cancelled_superseded_and_invalid_frames_do_not_gain_authority() {
    let source = "|a|b|\n|-|-|\nx|y\n";
    for authority in [
        GlobalSourceAuthority {
            current_revision: 19,
            cancelled: true,
        },
        GlobalSourceAuthority {
            current_revision: 20,
            cancelled: false,
        },
    ] {
        let mut producer = scanner(source);
        let before = producer.total_work();
        assert!(producer
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: 0,
                    bytes: source.as_bytes(),
                    real_eof: true
                },
                authority,
                4096
            )
            .is_err());
        assert_eq!(producer.committed_byte(), 0);
        assert_eq!(producer.total_work(), before);
    }
    let mut producer = scanner(source);
    assert!(producer
        .advance(
            SourceFrame {
                source_revision: 19,
                byte_start: 1,
                bytes: &source.as_bytes()[1..],
                real_eof: true
            },
            GlobalSourceAuthority {
                current_revision: 19,
                cancelled: false
            },
            4096
        )
        .is_err());
    assert_eq!(producer.total_work(), GlobalWork::default());
}

#[test]
fn budget_failure_and_cancel_cannot_be_retried_into_a_false_complete() {
    let source = "> > > > > |a|b|\n> > > > > |-|-|\n";
    let mut producer = scanner(source);
    producer.spine = StructuralSpineSnapshot::new(StructuralSpineLimits {
        max_depth: 4,
        max_live_page_bytes: 4 * 1024 * 1024,
        max_page_allocations: 100,
        max_role_work: 10000,
    })
    .unwrap();
    let first = loop {
        let start = producer.next_byte();
        let end = (start + 7).min(source.len());
        match producer.advance(
            SourceFrame {
                source_revision: 19,
                byte_start: start,
                bytes: &source.as_bytes()[start..end],
                real_eof: end == source.len(),
            },
            GlobalSourceAuthority {
                current_revision: 19,
                cancelled: false,
            },
            4096,
        ) {
            Err(error) => break error,
            Ok(GlobalStep::Fact { .. } | GlobalStep::Complete { .. }) => {
                panic!("预算未完成不能伪造结构事实或完成")
            }
            _ => (),
        }
    };
    assert_eq!(
        first,
        GlobalFrontierError::Spine(StructuralSpineError::Depth)
    );
    let start = producer.next_byte();
    let end = (start + 7).min(source.len());
    assert_eq!(
        producer
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len()
                },
                GlobalSourceAuthority {
                    current_revision: 19,
                    cancelled: false
                },
                4096
            )
            .unwrap_err(),
        first
    );
}

#[test]
fn deep_real_table_producer_keeps_shared_checkpoint_and_exact_source_mapping() {
    let quote = "> ".repeat(2048);
    let source = format!("{quote}| A | B |\r\n{quote}| - | - |\r\n{quote}| 中😀 | x |\r\n");
    let mut producer = scanner(&source);
    let mut saw_table = false;
    let mut saw_row = false;
    loop {
        let start = producer.next_byte();
        let end = (start + 17).min(source.len());
        let step = producer
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                GlobalSourceAuthority {
                    current_revision: 19,
                    cancelled: false,
                },
                2048,
            )
            .unwrap();
        let work = match &step {
            GlobalStep::Progress { work, .. }
            | GlobalStep::NeedInput { work, .. }
            | GlobalStep::Fact { work, .. }
            | GlobalStep::Complete { work } => work,
        };
        assert!(work.byte_reads <= 2048 && work.transitions <= 2048);
        match step {
            GlobalStep::Fact {
                fact: GlobalFact::Table(_),
                ..
            } => {
                saw_table = true;
                let first = producer.table_checkpoint().unwrap();
                let second = producer.table_checkpoint().unwrap();
                let a = TableContinuationState::resume(first);
                let b = TableContinuationState::resume(second);
                assert_eq!(a.next_byte(), b.next_byte());
                assert_eq!(a.buffered_source_capacity(), 0);
            }
            GlobalStep::Fact {
                fact: GlobalFact::Row(row),
                ..
            } => {
                saw_row = true;
                assert_eq!(
                    row.utf16_start,
                    source[..row.byte_start].encode_utf16().count()
                );
                assert_eq!(row.utf16_end, source[..row.byte_end].encode_utf16().count());
                assert_eq!(row.source_line, 2);
                assert_eq!(row.container_prefix_bytes, quote.len());
            }
            GlobalStep::Complete { .. } => break,
            _ => (),
        }
    }
    assert!(saw_table && saw_row);
    assert!(
        std::mem::size_of::<Phase>() < 1024,
        "phase不应每个小frame搬运4KiB页游标"
    );
}

#[test]
fn global_reference_facts_preserve_real_scope_backup_and_first_definition_order() {
    use crate::block_context::scan_containers;
    use crate::scanners::LineStart;
    for source in [
        "[α]: /one \"title\"\n[Α]: /two\n",
        "> [name\n>   label]: /url\n>   'line\n>   title'\n",
        "paragraph\n[r]: /not-definition\n\n[x]: /real\n",
        "```\n[fake]: /url\n```\n[real]: /url\n",
        "[r]: /url\n'not title' junk\n\n[next]: /next\n",
        "[r]: /url\n|a|b|\n|-|-|\nx|y\n[next]: /next\n",
        "- [r]: /url\n  'title'\n\n[next]: /next\n",
        "---\nx: yes\n---\n[r]: /url\n",
    ] {
        let parser = Parser::new_ext(source, dialect());
        let mut expected = parser
            .reference_definitions()
            .iter()
            .map(|(label, def)| (label.to_lowercase(), def.span.clone()))
            .collect::<Vec<_>>();
        expected.sort_by(|left, right| left.0.cmp(&right.0));
        let mut actual = std::collections::BTreeMap::new();
        for fact in consume(source, 1) {
            if let GlobalFact::Reference(reference) = fact {
                let candidate = reference.candidate;
                let raw = &source[candidate.label_start..candidate.label_end + 1];
                let label = candidate
                    .normalized_label(raw, |bytes| {
                        // 测试oracle拥有真实有限candidate slice；产品接线改用分段scope probe的预备prefix map。
                        let mut start = LineStart::new(bytes);
                        scan_containers(&reference.spine, &mut start, reference.options);
                        start.scan_space(4);
                        Some(start.bytes_scanned())
                    })
                    .unwrap();
                actual
                    .entry(label.to_lowercase())
                    .or_insert(candidate.span_start..candidate.span_end);
            }
        }
        assert_eq!(
            actual.into_iter().collect::<Vec<_>>(),
            expected,
            "{source:?}"
        );
    }
}

#[test]
fn verified_scope_probes_preserve_container_only_and_label_title_distinctions() {
    use crate::block_context::scan_containers;
    use crate::global_frontier::{ReferencePrefixPurpose, ReferencePrefixStep};
    use crate::scanners::LineStart;
    let source = "> [name\r\n>   label]: /url\r\n>   'title\r\n>     second'\r\n";
    let reference = consume(source, 1)
        .into_iter()
        .find_map(|fact| {
            if let GlobalFact::Reference(reference) = fact {
                Some(reference)
            } else {
                None
            }
        })
        .unwrap();
    for (index, byte) in source.bytes().enumerate() {
        if byte != b'\n' || index + 1 >= reference.candidate.span_end {
            continue;
        }
        let origin = index + 1;
        for purpose in [
            ReferencePrefixPurpose::ContainerOnly,
            ReferencePrefixPurpose::Label,
            ReferencePrefixPurpose::Whitespace,
            ReferencePrefixPurpose::Title,
        ] {
            let mut old = LineStart::new(&source.as_bytes()[origin..]);
            scan_containers(&reference.spine, &mut old, reference.options);
            if purpose != ReferencePrefixPurpose::ContainerOnly {
                old.scan_space(4);
            }
            if purpose == ReferencePrefixPurpose::Title {
                old.scan_all_space();
            }
            let expected = old.bytes_scanned();
            let mut probe = reference.prefix_scanner(origin, purpose).unwrap();
            loop {
                let start = probe.next_byte();
                let end = (start + 1).min(source.len());
                let step = probe
                    .advance(
                        SourceFrame {
                            source_revision: 19,
                            byte_start: start,
                            bytes: &source.as_bytes()[start..end],
                            real_eof: end == source.len(),
                        },
                        GlobalSourceAuthority {
                            current_revision: 19,
                            cancelled: false,
                        },
                        2048,
                    )
                    .unwrap();
                let work = match step {
                    ReferencePrefixStep::Progress { work, .. }
                    | ReferencePrefixStep::NeedInput { work, .. }
                    | ReferencePrefixStep::Complete { work, .. } => work,
                };
                assert!(work.byte_reads <= 2048 && work.transitions <= 2048);
                if let ReferencePrefixStep::Complete { prefix_bytes, .. } = step {
                    assert_eq!(prefix_bytes, expected);
                    break;
                }
            }
        }
    }
    // CRLF中间与正文中间都不是合法续行锚点，不能利用任意slice起点去掉正文。
    for origin in [4, source.find('\r').unwrap() + 1] {
        let mut probe = reference
            .prefix_scanner(origin, ReferencePrefixPurpose::Label)
            .unwrap();
        let mut rejected = false;
        for _ in 0..3 {
            let start = probe.next_byte();
            let end = (start + 1).min(source.len());
            if probe
                .advance(
                    SourceFrame {
                        source_revision: 19,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..end],
                        real_eof: end == source.len(),
                    },
                    GlobalSourceAuthority {
                        current_revision: 19,
                        cancelled: false,
                    },
                    2048,
                )
                .is_err()
            {
                rejected = true;
                break;
            }
        }
        assert!(rejected);
    }
}

#[test]
fn verified_table_end_keeps_physical_scope_and_original_ordinal_without_source_payload() {
    for source in [
        "| H |\n| - |\n",
        "| H |\r\n| - |\r\n| 中😀 |\r\n",
        "| H |\n| - |\n| body |",
        "| H |\n| - |",
        "- > | H |\n  > | - |\n  > | 中😀 |\n\noutside\n",
        "[^a]: | H |\n    | - |\n    | body |\n\noutside\n",
        "| H |\n| - |\n|\nnext\n",
    ] {
        let facts = consume(source, 1);
        let tables = facts
            .iter()
            .filter_map(|fact| {
                if let GlobalFact::Table(table) = fact {
                    Some(table)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let ends = facts
            .iter()
            .filter_map(|fact| {
                if let GlobalFact::TableEnd(end) = fact {
                    Some(end)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(tables.len(), 1, "{source:?}");
        assert_eq!(ends.len(), 1, "结束事实只发一次 {source:?}");
        let table = tables[0];
        let end = ends[0];
        let rows = facts
            .iter()
            .filter_map(|fact| {
                if let GlobalFact::Row(row) = fact {
                    Some(row)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(end.header_byte_start(), table.header_byte_start);
        assert_eq!(end.columns(), table.columns);
        assert_eq!(
            end.last_body_row_ordinal(),
            rows.last().map(|row| row.row_ordinal)
        );
        assert_eq!(
            end.continuation_prefix_byte_range(),
            table.separator_byte_start..table.separator_byte_start + table.separator_prefix_bytes
        );
        assert_eq!(end.ended_at_source_eof(), end.byte_end() == source.len());
        let prefix = &source[end.continuation_prefix_byte_range()];
        if source.starts_with("- >") {
            assert_eq!(
                prefix, "  > ",
                "追加复用D续行，不把H的list opener复制成新item"
            );
            assert!(end.container_depth() >= 4);
        }
        if source.starts_with("[^a]") {
            assert_eq!(prefix, "    ", "footnote continuation保留原缩进");
        }
        compare(source, 1);
    }
}

#[test]
fn declared_source_end_without_real_eof_never_publishes_table_end_or_complete() {
    let source = "| H |\n| - |\n| body |\n";
    let mut producer = scanner(source);
    let authority = GlobalSourceAuthority {
        current_revision: 19,
        cancelled: false,
    };
    let mut table_count = 0;
    let mut row_count = 0;
    let mut waited = false;
    for _ in 0..10000 {
        let start = producer.next_byte();
        let end = (start + 1).min(source.len());
        let step = producer
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: false,
                },
                authority,
                4096,
            )
            .unwrap();
        match step {
            GlobalStep::Fact {
                fact: GlobalFact::Table(_),
                ..
            } => table_count += 1,
            GlobalStep::Fact {
                fact: GlobalFact::Row(_),
                ..
            } => row_count += 1,
            GlobalStep::Fact {
                fact: GlobalFact::TableEnd(_),
                ..
            }
            | GlobalStep::Complete { .. } => panic!("未交付真实EOF不能签发表尾/完整扫描"),
            GlobalStep::NeedInput { next_byte, .. } if next_byte == source.len() => {
                waited = true;
                break;
            }
            _ => (),
        }
    }
    assert!(waited);
    assert_eq!((table_count, row_count), (1, 1));
    let mut end_count = 0;
    for _ in 0..10000 {
        let start = producer.next_byte();
        let end = (start + 1).min(source.len());
        match producer
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority,
                4096,
            )
            .unwrap()
        {
            GlobalStep::Fact {
                fact: GlobalFact::TableEnd(end),
                ..
            } => {
                assert!(end.ended_at_source_eof());
                assert_eq!(end.last_body_row_ordinal(), Some(1));
                end_count += 1;
            }
            GlobalStep::Complete { .. } => {
                assert_eq!(end_count, 1);
                return;
            }
            _ => (),
        }
    }
    panic!("真实EOF交付后应有界完成");
}

#[test]
fn classified_watermark_never_crosses_pending_header_or_partial_commit() {
    let source = format!(
        "| {} |\r\n| - |\r\n| v |\r\n\r\ntext\r\n",
        "中".repeat(3000)
    );
    let delimiter_end = source.find("| v |").unwrap();
    let mut global = scanner(&source);
    let mut last = (0, 0);
    let mut table_seen = false;
    let mut partial_commit_seen = false;
    for _ in 0..source.len() * 100 + 10000 {
        let start = global.next_byte();
        let end = (start + 1).min(source.len());
        let step = global
            .advance(
                SourceFrame {
                    source_revision: 19,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                GlobalSourceAuthority {
                    current_revision: 19,
                    cancelled: false,
                },
                4096,
            )
            .unwrap();
        if matches!(
            step,
            GlobalStep::Fact {
                fact: GlobalFact::Table(_),
                ..
            }
        ) {
            table_seen = true;
        }
        let classified = global.table_classified_position();
        assert!(classified.0 >= last.0 && classified.1 >= last.1);
        if !table_seen {
            assert_eq!(classified, (0, 0));
        }
        if global.committed_byte() > classified.0 {
            partial_commit_seen = true;
        }
        if table_seen {
            assert!(classified.0 >= delimiter_end);
        }
        last = classified;
        if matches!(step, GlobalStep::Complete { .. }) {
            assert!(partial_commit_seen && table_seen);
            assert_eq!(classified, (source.len(), source.encode_utf16().count()));
            return;
        }
    }
    panic!("真实分类水位夹具未完成");
}

#[test]
fn dense_paragraph_under_original_core_fuel_does_not_charge_unused_workspace_reserves() {
    let source = (0..300)
        .map(|line| format!("x{line}\n"))
        .collect::<String>();
    let expected = Parser::new_ext(&source, dialect())
        .into_offset_iter()
        .filter_map(|(event, range)| matches!(event, Event::Start(Tag::Paragraph)).then_some(range))
        .collect::<Vec<_>>();
    for width in [1, 2, 7, 4096] {
        // 与 Core 原有总限额完全相同，不通过增大 fuel 掩盖固定预留被重复收费的问题。
        let mut global = GlobalFrontier::new(
            19,
            source.len(),
            dialect(),
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: source.len() * 2048 + 1024 * 1024,
                },
                max_byte_work: source.len() * 256 + 1024 * 1024,
                max_transitions: source.len() * 2048 + 1024 * 1024,
            },
        )
        .unwrap();
        let mut actual = Vec::new();
        let mut completed = false;
        let mut scalar_only = false;
        for _ in 0..source.len() * 100 + 10000 {
            let start = global.next_byte();
            let end = (start + width).min(source.len());
            let prefix_before = global.prefix.is_some();
            let step = global
                .advance(
                    SourceFrame {
                        source_revision: 19,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..end],
                        real_eof: end == source.len(),
                    },
                    GlobalSourceAuthority {
                        current_revision: 19,
                        cancelled: false,
                    },
                    3007,
                )
                .unwrap();
            let work = match &step {
                GlobalStep::Progress { work, .. }
                | GlobalStep::NeedInput { work, .. }
                | GlobalStep::Fact { work, .. }
                | GlobalStep::Complete { work } => *work,
            };
            assert!(work.byte_reads <= 3007 && work.transitions <= 3007);
            assert_eq!(
                global.step_prefix_workspace_work != 0,
                prefix_before != global.prefix.is_some(),
                "只有真实 page-pointer 游标初始化或释放才消费固定 workspace 费用"
            );
            scalar_only |= global.step_prefix_workspace_work == 0
                && work.byte_reads == 0
                && work.transitions < PREFIX_WORKSPACE_WORK;
            match step {
                GlobalStep::Fact {
                    fact: GlobalFact::InlineBlock(block),
                    ..
                } => actual.push(block.source_range()),
                GlobalStep::Complete { .. } => {
                    completed = true;
                    break;
                }
                _ => (), // 中间行/控制阶段没有独立 paragraph 或普通样式权限。
            }
        }
        assert!(completed && scalar_only);
        assert_eq!(
            actual, expected,
            "真实完整 Parser 闭合范围不能随费用修正改变"
        );
        assert_eq!(
            global.table_classified_position(),
            (source.len(), source.encode_utf16().count())
        );
    }
}

#[test]
fn minimum_global_step_pays_ready_prefix_release_and_reference_need_line_without_overrun() {
    let source = "> [name\r\n> label]: /url\r\n> 'line\r\n> title'\r\n\r\ntext\r\n\r\n| H |\r\n| - |\r\n| v |\r\n";
    for width in [1, 2, 7, 4096] {
        let mut global = scanner(source);
        let mut saw_ready_init = false;
        let mut saw_prefix_release = false;
        let mut saw_reference_init = false;
        let mut saw_separator_init = false;
        let mut saw_separator_release = false;
        let mut complete = false;
        for _ in 0..10000 {
            let ready = matches!(global.phase, Phase::Ready(Ready::Begin));
            let reference = matches!(global.phase, Phase::Reference(_));
            let separator = matches!(global.phase, Phase::Prefix(PrefixUse::Separator));
            let prefix_before = global.prefix.is_some();
            let start = global.next_byte();
            let end = (start + width).min(source.len());
            let step = global
                .advance(
                    SourceFrame {
                        source_revision: 19,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..end],
                        real_eof: end == source.len(),
                    },
                    GlobalSourceAuthority {
                        current_revision: 19,
                        cancelled: false,
                    },
                    2048,
                )
                .unwrap();
            let work = match &step {
                GlobalStep::Progress { work, .. }
                | GlobalStep::NeedInput { work, .. }
                | GlobalStep::Fact { work, .. }
                | GlobalStep::Complete { work } => *work,
            };
            assert!(
                work.byte_reads <= 2048 && work.transitions <= 2048,
                "最小量子必须为控制与真实工作区组合预先留足额度"
            );
            if global.step_prefix_workspace_work != 0 {
                assert!(work.transitions >= PREFIX_WORKSPACE_WORK + GLOBAL_CONTROL_WORK);
                saw_ready_init |= ready && !prefix_before && global.prefix.is_some();
                saw_prefix_release |= prefix_before && global.prefix.is_none();
                saw_reference_init |= reference && !prefix_before && global.prefix.is_some();
                saw_separator_init |=
                    !prefix_before && matches!(global.phase, Phase::Prefix(PrefixUse::Separator));
                saw_separator_release |= separator && global.prefix.is_none();
            }
            if matches!(step, GlobalStep::Complete { .. }) {
                complete = true;
                break;
            }
        }
        assert!(
            complete
                && saw_ready_init
                && saw_prefix_release
                && saw_reference_init
                && saw_separator_init
                && saw_separator_release,
            "必须实际经过 Ready、Prefix 完成、Reference NeedLine 与表格 Separator 的创建/释放路径"
        );
    }
}
