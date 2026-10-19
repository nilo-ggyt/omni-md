// Copyright 2015 Google Inc. All rights reserved.
// 合成oracle复用原firstpass与public offset事件；完整MIT条款见LICENSE。

use super::*;
use crate::{
    block_frontier::SourceFrame,
    global_frontier::{
        GlobalFact, GlobalFrontier, GlobalFrontierError, GlobalFrontierLimits,
        GlobalSourceAuthority, GlobalStep, StructuralSpineLimits,
    },
    Event, Parser, Tag,
};

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

fn global(source: &str) -> GlobalFrontier {
    GlobalFrontier::new(
        71,
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

fn consume(source: &str, width: usize) -> Vec<VerifiedGlobalInlineBlock> {
    let mut producer = global(source);
    let mut blocks = Vec::new();
    for _ in 0..source.len().saturating_mul(100) + 10000 {
        let start = producer.next_byte();
        let end = start.saturating_add(width).min(source.len());
        let step = producer
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                GlobalSourceAuthority {
                    current_revision: 71,
                    cancelled: false,
                },
                4096,
            )
            .unwrap_or_else(|error| panic!("{error:?} source={source:?} start={start}"));
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
                assert_eq!(block.source_revision(), 71);
                assert_eq!(block.source_end(), source.len());
                assert!(block.physical_range().start <= block.source_range().start);
                assert!(block.lexical_range().end <= block.source_range().end);
                assert!(block.terminal_eol_range().end <= block.lexical_range().end);
                blocks.push(block);
            }
            GlobalStep::Complete { .. } => return blocks,
            _ => (), // table/ref事实保持原算法；不由其数量推断普通块完成。
        }
    }
    panic!("真实块续体没有单调完成");
}

fn compare(source: &str, width: usize) {
    let expected = crate::firstpass::capture_closed_inline_blocks(source, dialect());
    let actual = consume(source, width)
        .iter()
        .map(|block| (block.kind(), block.source_range(), block.lexical_range()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "frame={width} source={source:?}");
}

#[test]
fn closed_inline_authorities_match_original_firstpass_completion_and_public_offsets() {
    let cases = [
        "ordinary",
        "ordinary\n",
        "a\r\nb\r\n\r\nnext",
        "a\\\n---\n",
        "a  \n\n",
        "a\\\n\nnext",
        "a\\\r\n# next\r\n",
        "a\n===\nnext",
        "#literal\n---\n",
        "a\n|b|c|\n|-|-|\nx|y\n",
        "a\nb|c\n-|-\nx|y\n",
        "|a|b|\n|-|-|\nx|y\n",
        "> a\nb lazy\n\n> c\n",
        "- a\n  body\n\n- second\n",
        "- [x] task\n  tail\n",
        "> [!NOTE]\n> body\ncontinued\n",
        "[r]: /first 'title'\n\n[r]: /second\n[x][r]\n",
        "[r]: /first\n  'not title\n# next\n",
        "[Straße]: /one\n[STRASSE]: /two\n\n[x][MASSE]\n",
        "x[^f]\n\n[^f]: note\n    continued\n",
        "```\nnot paragraph\n```\nreal\n",
        "<script>\nnot paragraph\n</script>\nreal\n",
        "---\nkey: value\n---\nreal\n",
    ];
    for source in cases {
        for width in [1, 2, 7, 4096, 65536] {
            compare(source, width);
        }
    }
    // 根级public事件是第二个独立出口；不能仅让新producer与测试夹具彼此镜像。
    for source in ["one\n\ntwo", "one\n===\n\ntwo", "one\r\n---\r\n"] {
        let expected = Parser::new_ext(source, dialect())
            .into_offset_iter()
            .filter_map(|(event, range)| match event {
                Event::Start(Tag::Paragraph) => Some((InlineBlockKind::Paragraph, range)),
                Event::Start(Tag::Heading { level, .. }) => {
                    Some((InlineBlockKind::SetextHeading(level), range))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let actual = consume(source, 1)
            .into_iter()
            .map(|block| (block.kind(), block.source_range()))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}

#[test]
fn block_transitions_randomly_match_original_completion_without_frame_eof() {
    let lines = [
        "a\n",
        "\n",
        "> q\n",
        "tail\n",
        "- item\n",
        "  in\n",
        "---\n",
        "===\n",
        "|a|b|\n",
        "|-|-|\n",
        "x|y\n",
        "[r]: /url\n",
        "[^f]: foot\n",
        "# h\n",
        "```\n",
        "<script>\n",
        "</script>\n",
        "a\\\n",
        "中😀\r\n",
        "- [x] t\n",
    ];
    let mut random = 0x79f1_aea0_512du64;
    for case in 0..1024 {
        let mut source = String::new();
        for _ in 0..case % 17 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            source.push_str(lines[random as usize % lines.len()]);
        }
        compare(&source, if case % 2 == 0 { 1 } else { 13 });
    }
}

#[test]
fn existing_upstream_block_inputs_match_original_closed_leaf_capture() {
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
            // 只读取上游原有合成源；预期始终来自真实firstpass而非该fixture的HTML输出。
            let source = item.split("\"##;").next().unwrap();
            compare(source, 3);
            count += 1;
        }
    }
    assert!(count > 600);
}

#[test]
fn giant_and_deep_paragraphs_keep_one_scope_and_exact_completed_extent() {
    for source in [
        format!("{}\n\n|a|b|\n|-|-|\nx|y\n", "中😀".repeat(200_000)),
        format!("{}\n---\n", "x".repeat(1024 * 1024)),
        format!("{}a\n\n", "> ".repeat(2048)),
    ] {
        compare(&source, 8191);
    }
    let source = format!("{}a", "> ".repeat(2048));
    let blocks = consume(&source, 7);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].scope_depth(), 2049);
    assert_eq!(blocks[0].source_range(), source.len() - 1..source.len());
}

#[test]
fn fake_eof_cancel_and_fuel_cannot_publish_a_partial_paragraph() {
    let source = "ordinary";
    let mut producer = global(source);
    let mut waiting = false;
    for _ in 0..100 {
        let start = producer.next_byte();
        let step = producer
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..],
                    real_eof: false,
                },
                GlobalSourceAuthority {
                    current_revision: 71,
                    cancelled: false,
                },
                4096,
            )
            .unwrap();
        assert!(!matches!(
            step,
            GlobalStep::Fact {
                fact: GlobalFact::InlineBlock(_),
                ..
            }
        ));
        if matches!(step, GlobalStep::NeedInput { .. }) && producer.next_byte() == source.len() {
            waiting = true;
            break;
        }
    }
    assert!(waiting);
    let start = producer.next_byte();
    assert_eq!(
        producer
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &[],
                    real_eof: true,
                },
                GlobalSourceAuthority {
                    current_revision: 71,
                    cancelled: true
                },
                4096
            )
            .unwrap_err(),
        GlobalFrontierError::Cancelled
    );
    assert_eq!(
        producer
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &[],
                    real_eof: true,
                },
                GlobalSourceAuthority {
                    current_revision: 71,
                    cancelled: false
                },
                4096
            )
            .unwrap_err(),
        GlobalFrontierError::Cancelled
    );

    let mut limited = GlobalFrontier::new(
        71,
        source.len(),
        dialect(),
        GlobalFrontierLimits {
            spine: StructuralSpineLimits {
                max_depth: 128,
                max_live_page_bytes: 65536,
                max_page_allocations: 10000,
                max_role_work: 100000,
            },
            max_byte_work: 100000,
            max_transitions: 1024,
        },
    )
    .unwrap();
    assert_eq!(
        limited
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: 0,
                    bytes: source.as_bytes(),
                    real_eof: true,
                },
                GlobalSourceAuthority {
                    current_revision: 71,
                    cancelled: false
                },
                4096
            )
            .unwrap_err(),
        GlobalFrontierError::WorkFuel
    );
}
