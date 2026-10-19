// Copyright 2015 Google Inc. All rights reserved.
// 合成差分使用原 firstpass/public offset 出口；完整 MIT 条款见 LICENSE。

use super::*;
use crate::{
    block_frontier::SourceFrame,
    firstpass::leaf_line_facts::{fence_info_bounds, heading_bounds, HeadingBoundsScanner},
    global_frontier::{
        GlobalFact, GlobalFrontier, GlobalFrontierError, GlobalFrontierLimits,
        GlobalSourceAuthority, GlobalStep, StructuralSpineLimits,
    },
    CodeBlockKind, Event, Parser, Tag,
};

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_GFM
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
}

fn producer(source: &str) -> GlobalFrontier {
    GlobalFrontier::new(
        93,
        source.len(),
        options(),
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

fn consume(source: &str, width: usize) -> (Vec<VerifiedGlobalLeafBlock>, usize) {
    let mut global = producer(source);
    let mut leaves = Vec::new();
    let mut tables = 0;
    let mut classified_before = (0, 0);
    for _ in 0..source.len().saturating_mul(100) + 10_000 {
        let start = global.next_byte();
        let end = start.saturating_add(width).min(source.len());
        let step = global
            .advance(
                SourceFrame {
                    source_revision: 93,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                GlobalSourceAuthority {
                    current_revision: 93,
                    cancelled: false,
                },
                4096,
            )
            .unwrap_or_else(|error| panic!("{error:?}, byte={start}, source={source:?}"));
        let work = match &step {
            GlobalStep::Progress { work, .. }
            | GlobalStep::NeedInput { work, .. }
            | GlobalStep::Fact { work, .. }
            | GlobalStep::Complete { work } => work,
        };
        assert!(work.byte_reads <= 4096 && work.transitions <= 4096);
        let classified = global.table_classified_position();
        assert!(classified.0 >= classified_before.0 && classified.1 >= classified_before.1);
        classified_before = classified;
        match step {
            GlobalStep::Fact {
                fact: GlobalFact::LeafBlock(leaf),
                ..
            } => {
                assert_eq!(leaf.source_revision(), 93);
                assert_eq!(leaf.source_end(), source.len());
                assert!(leaf.physical_start() <= leaf.source_range().start);
                // 真实 HTML item 可含后继 prefix；只核原 consumed cursor 已提交，绝不扩大无表水位。
                assert!(leaf.consumed_end() <= classified.0);
                leaves.push(leaf);
            }
            GlobalStep::Fact {
                fact: GlobalFact::Table(_),
                ..
            } => tables += 1,
            GlobalStep::Complete { .. } => return (leaves, tables),
            _ => (), // 其他真实事实仍经过同一个 producer，不能由叶块数推断普通语义 Ready。
        }
    }
    panic!("真实叶块未在有限步内完成");
}

fn compare(source: &str, width: usize) {
    let expected = crate::firstpass::capture_closed_leaf_blocks(source, options());
    let (leaves, _) = consume(source, width);
    let actual = leaves
        .iter()
        .map(|leaf| (leaf.kind(), leaf.source_range(), leaf.consumed_end()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "frame={width}, source={source:?}");
    // public offset 是独立于捕获字段的第二个真实出口；ATX 与 Setext 由原 firstpass 捕获种类区分。
    let offsets = Parser::new_ext(source, options())
        .into_offset_iter()
        .filter_map(|(event, range)| {
            let kind = match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    if !expected.iter().any(|(kind, extent, _)| {
                        *kind == LeafBlockKind::AtxHeading(level) && *extent == range
                    }) {
                        return None;
                    }
                    LeafBlockKind::AtxHeading(level)
                }
                Event::Rule => LeafBlockKind::ThematicBreak,
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => LeafBlockKind::FencedCode,
                Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)) => {
                    LeafBlockKind::IndentedCode
                }
                Event::Start(Tag::HtmlBlock) => LeafBlockKind::Html,
                Event::Start(Tag::MetadataBlock(kind)) => LeafBlockKind::Metadata(kind),
                _ => return None,
            };
            Some((kind, range))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual
            .iter()
            .map(|(kind, range, _)| (*kind, range.clone()))
            .collect::<Vec<_>>(),
        offsets
    );
}

#[test]
fn completed_leaf_ranges_and_original_return_cursors_survive_small_frames_and_containers() {
    for source in [
        "# 中😀 ### \r\n***\r\n",
        "#\n##   \r\n###### t\n",
        "# 标题 ### {#标识 .样式}\n",
        "``` rust\\+lang \t\r\n中😀\r\n````  \r\nnext\n",
        "~~~  a`b  \nbody\n~~~\n",
        "```\nbody",
        "```",
        "<script>\n中😀\n</script>\nnext\n",
        "<!-- open\nend -->\r\nnext\n",
        "<div>\nbody\n\nnext\n",
        "<custom attr='v'>\nbody\n\nnext\n",
        "    first\r\n\r\n    second\r\n\r\n\r\nnext\n",
        "    first\n\n   ",
        "---\nkey: value\n...  \r\nnext\n",
        "+++\nkey: value\n+++\nnext\n",
        "> ``` rust\n> body\n> ```\n> # heading\n",
        "> <script>\n> body\n> ",
        "> <div>\n> body\n> ",
        "> <div>\n> body\n>   \n> # after\n",
        "> <div>\n> body\n> \u{000b}\n> # after\n",
        "- ```\n  code\n  ```\n- # next\n",
        "-     code\n\n      tail\n\n",
        "[^f]: # 标题\n\n    ***\n",
        "> ---\n> key: value\noutside\n---\n\n|a|b|\n|-|-|\nx|y\n",
    ] {
        for width in [1, 2, 7, 4096] {
            compare(source, width);
        }
    }
}

#[test]
fn producer_owned_payloads_keep_info_content_markers_and_real_close_reasons() {
    let source =
        "# 标题 ### {#id}\r\n\n``` rust\\+lang \t\r\n中😀\r\n````  \r\n\n    code\n\n\nnext\n";
    let (leaves, _) = consume(source, 1);
    let atx = &leaves[0];
    let LeafBlockDetails::AtxHeading {
        opening_marker,
        lexical_content,
        trim_target,
        closing_hashes,
        attributes,
    } = atx.details()
    else {
        panic!("ATX 凭证种类错误");
    };
    assert_eq!(&source[opening_marker.clone()], "#");
    assert_eq!(&source[lexical_content.clone()], "标题 ### ");
    assert_eq!(&source[lexical_content.start..*trim_target], "标题");
    assert_eq!(&source[closing_hashes.clone().unwrap()], "###");
    assert_eq!(&source[attributes.clone().unwrap()], "#id");
    let LeafBlockDetails::FencedCode {
        opening_marker,
        info,
        body_enclosure,
        closing_marker,
        closing_scanner_end,
        ..
    } = leaves[1].details()
    else {
        panic!("fence 凭证种类错误");
    };
    assert_eq!(&source[opening_marker.clone()], "```");
    assert_eq!(&source[info.clone()], "rust\\+lang");
    assert_eq!(&source[body_enclosure.clone()], "中😀\r\n");
    assert_eq!(&source[closing_marker.clone().unwrap()], "````");
    assert_eq!(leaves[1].source_range().end, closing_scanner_end.unwrap());
    assert!(leaves[1].consumed_end() > leaves[1].source_range().end);
    assert_eq!(
        leaves[1].close_reason(),
        LeafBlockCloseReason::ExplicitMarker
    );
    let LeafBlockDetails::IndentedCode {
        last_nonblank_end,
        body_enclosure,
        ..
    } = leaves[2].details()
    else {
        panic!("indented 凭证种类错误");
    };
    assert_eq!(leaves[2].source_range().end, *last_nonblank_end);
    assert_eq!(&source[body_enclosure.clone()], "code\n");
    assert!(leaves[2].consumed_end() > *last_nonblank_end);
    let (unclosed, _) = consume("```\n中😀", 1);
    assert_eq!(unclosed[0].close_reason(), LeafBlockCloseReason::TrueEof);
    assert!(matches!(
        unclosed[0].details(),
        LeafBlockDetails::FencedCode {
            closing_marker: None,
            closing_scanner_end: None,
            ..
        }
    ));
    let (html, _) = consume("<script>中😀</script>", 2);
    assert_eq!(html[0].close_reason(), LeafBlockCloseReason::HtmlTerminator);
    assert!(
        matches!(html[0].details(), LeafBlockDetails::Html { terminator: Some(range), .. } if &"<script>中😀</script>"[range.clone()] == "</script>")
    );
}

#[test]
fn opaque_scope_exit_and_metadata_lookahead_never_hide_later_real_tables() {
    for source in [
        "> ```\n> |fake|table|\n> |-|-|\noutside\n\n|a|b|\n|-|-|\nx|y\n",
        "> ---\n> key: value\noutside\n---\n\n|a|b|\n|-|-|\nx|y\n",
        "<script>\n|fake|table|\n|-|-|\n</script>\n\n|a|b|\n|-|-|\nx|y\n",
    ] {
        let (leaves, tables) = consume(source, 1);
        assert_eq!(tables, 1);
        assert!(leaves
            .iter()
            .all(|leaf| leaf.source_range().end <= source.find("|a|b|").unwrap()));
        compare(source, 3);
    }
    let source = "> <script>\n> body\n> ";
    let (leaves, _) = consume(source, 1);
    assert_eq!(leaves[0].source_range().end, source.len());
    assert!(leaves[0].consumed_end() < leaves[0].source_range().end);
    let source = "> <div>\n> body\n>   \n> |a|b|\n> |-|-|\n> x|y\n";
    let mut global = producer(source);
    let mut observed = false;
    for _ in 0..10_000 {
        let start = global.next_byte();
        let end = (start + 1).min(source.len());
        let step = global
            .advance(
                SourceFrame {
                    source_revision: 93,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                GlobalSourceAuthority {
                    current_revision: 93,
                    cancelled: false,
                },
                4096,
            )
            .unwrap();
        if let GlobalStep::Fact {
            fact: GlobalFact::LeafBlock(leaf),
            ..
        } = step
        {
            assert_eq!(leaf.kind(), LeafBlockKind::Html);
            assert!(leaf.source_range().end > leaf.consumed_end());
            assert_eq!(global.table_classified_position().0, leaf.consumed_end());
            observed = true;
            break;
        }
    }
    assert!(observed);
}

#[test]
fn incomplete_frames_cancel_and_supersede_do_not_publish_a_half_leaf() {
    for source in [
        "# heading",
        "``` lang\nbody",
        "<script>body",
        "    body",
        "---\nkey: value\n---",
    ] {
        let mut global = producer(source);
        for _ in 0..10_000 {
            let start = global.next_byte();
            let end = (start + 1).min(source.len());
            let step = global
                .advance(
                    SourceFrame {
                        source_revision: 93,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..end],
                        real_eof: false,
                    },
                    GlobalSourceAuthority {
                        current_revision: 93,
                        cancelled: false,
                    },
                    4096,
                )
                .unwrap();
            assert!(!matches!(
                step,
                GlobalStep::Fact {
                    fact: GlobalFact::LeafBlock(_),
                    ..
                }
            ));
            if start == source.len() {
                break;
            }
        }
        let start = global.next_byte();
        let frame = || SourceFrame {
            source_revision: 93,
            byte_start: start,
            bytes: &source.as_bytes()[start..start],
            real_eof: start == source.len(),
        };
        assert_eq!(
            global
                .advance(
                    frame(),
                    GlobalSourceAuthority {
                        current_revision: 93,
                        cancelled: true
                    },
                    4096
                )
                .unwrap_err(),
            GlobalFrontierError::Cancelled
        );
        assert_eq!(
            global
                .advance(
                    frame(),
                    GlobalSourceAuthority {
                        current_revision: 93,
                        cancelled: false
                    },
                    4096
                )
                .unwrap_err(),
            GlobalFrontierError::Cancelled
        );
    }
    let mut global = producer("```\nbody");
    let frame = || SourceFrame {
        source_revision: 93,
        byte_start: 0,
        bytes: b"",
        real_eof: false,
    };
    assert_eq!(
        global
            .advance(
                frame(),
                GlobalSourceAuthority {
                    current_revision: 94,
                    cancelled: false
                },
                4096
            )
            .unwrap_err(),
        GlobalFrontierError::SourceSuperseded
    );
    assert_eq!(
        global
            .advance(
                frame(),
                GlobalSourceAuthority {
                    current_revision: 93,
                    cancelled: false
                },
                4096
            )
            .unwrap_err(),
        GlobalFrontierError::SourceSuperseded
    );
    let source = format!("``` lang\n{}", "x".repeat(4096));
    let mut global = GlobalFrontier::new(
        93,
        source.len(),
        options(),
        GlobalFrontierLimits {
            spine: StructuralSpineLimits {
                max_depth: 16_384,
                max_live_page_bytes: 4 * 1024 * 1024,
                max_page_allocations: 1_000_000,
                max_role_work: 200_000_000,
            },
            max_byte_work: 64,
            max_transitions: 1_000_000,
        },
    )
    .unwrap();
    let mut exhausted = false;
    for _ in 0..10_000 {
        let start = global.next_byte();
        let end = (start + 1).min(source.len());
        let result = global.advance(
            SourceFrame {
                source_revision: 93,
                byte_start: start,
                bytes: &source.as_bytes()[start..end],
                real_eof: end == source.len(),
            },
            GlobalSourceAuthority {
                current_revision: 93,
                cancelled: false,
            },
            4096,
        );
        match result {
            Err(error) => {
                assert_eq!(error, GlobalFrontierError::WorkFuel);
                exhausted = true;
                let next = global.next_byte();
                assert_eq!(
                    global
                        .advance(
                            SourceFrame {
                                source_revision: 93,
                                byte_start: next,
                                bytes: &[],
                                real_eof: next == source.len()
                            },
                            GlobalSourceAuthority {
                                current_revision: 93,
                                cancelled: false
                            },
                            4096
                        )
                        .unwrap_err(),
                    GlobalFrontierError::WorkFuel
                );
                break;
            }
            Ok(step) => assert!(!matches!(
                step,
                GlobalStep::Fact {
                    fact: GlobalFact::LeafBlock(_),
                    ..
                }
            )),
        }
    }
    assert!(exhausted);
    // 旧 blank 入口反例现在沿同一真实规则完成，不能因新增 leaf 功能使原表格请求失败。
    for source in [
        "    \u{000b}\n",
        "\u{000c}\rnext\n",
        "- \u{000b}\r\n- # next\n",
        "> \u{000c}\r\n> # next\n",
        "    \u{000b}\n|a|b|\n|-|-|\nx|y\n",
    ] {
        compare(source, 1);
    }
}

#[test]
fn giant_leaf_records_and_retained_scope_release_without_source_or_row_payloads() {
    assert!(std::mem::size_of::<PendingGlobalLeaf>() < 1024);
    assert!(std::mem::size_of::<VerifiedGlobalLeafBlock>() < 512);
    for source in [
        format!("``` {}\n中😀\n```\n", "x".repeat(1024 * 1024)),
        format!("<script>\n{}\n</script>\n", "x".repeat(1024 * 1024)),
        format!("    {}\n\n", "x".repeat(1024 * 1024)),
    ] {
        compare(&source, 8191);
    }
    let source = format!("{}# deep\n", "> ".repeat(2048));
    let (mut leaves, _) = consume(&source, 7);
    assert_eq!(leaves[0].scope_depth(), 2048);
    let mut budget_observer = leaves[0].spine.clone();
    while !budget_observer.is_empty() {
        budget_observer.pop().unwrap();
    }
    assert!(budget_observer.payload_counts().0 > 0);
    leaves.clear();
    assert_eq!(budget_observer.payload_counts().0, 0);
}

#[test]
fn shared_heading_and_fence_bounds_match_the_original_reverse_rules() {
    fn old_heading(bytes: &[u8], attributes: bool) -> (usize, usize, Option<Range<usize>>) {
        let mut content_end = bytes.len();
        let mut attrs = None;
        if attributes {
            let close = bytes
                .iter()
                .rposition(|byte| !matches!(byte, b'\n' | b'\r' | b' ' | b'\t'));
            if let Some(close) = close.filter(|&position| bytes[position] == b'}') {
                if let Some(open) = bytes[..close].iter().rposition(|byte| {
                    matches!(byte, b'{' | b'}' | b'<' | b'>' | b'\\' | b'\n' | b'\r')
                }) {
                    if bytes[open] == b'{' {
                        content_end = open;
                        attrs = Some(open + 1..close);
                    }
                }
            }
        }
        let mut limit = bytes[..content_end]
            .iter()
            .rposition(|byte| !matches!(byte, b'\n' | b'\r' | b' '))
            .map_or(0, |position| position + 1);
        let closer = bytes[..limit]
            .iter()
            .rposition(|&byte| byte != b'#')
            .map_or(0, |position| position + 1);
        if closer == 0 {
            limit = 0;
        } else {
            let spaces = bytes[..closer]
                .iter()
                .rev()
                .take_while(|&&byte| byte == b' ')
                .count();
            if spaces > 0 {
                limit = closer - spaces;
            }
        }
        (content_end, limit, attrs)
    }
    let alphabet = b"a# {}\\<>\t\n\r\x0b";
    let mut random = 0x73c4_9a12_f005u64;
    for case in 0..4096 {
        let mut bytes = Vec::new();
        for _ in 0..case % 73 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            bytes.push(alphabet[random as usize % alphabet.len()]);
        }
        for attributes in [false, true] {
            let actual = heading_bounds(&bytes, attributes);
            assert_eq!(
                (actual.content_end, actual.trim_end, actual.attributes),
                old_heading(&bytes, attributes),
                "bytes={bytes:?}"
            );
            let mut scanner = HeadingBoundsScanner::default();
            for byte in &bytes {
                scanner.observe(&[*byte]);
            }
            assert_eq!(
                scanner.finish(attributes),
                heading_bounds(&bytes, attributes)
            );
        }
        let leading = bytes
            .iter()
            .take_while(|&&byte| is_ascii_whitespace_no_nl(byte))
            .count();
        let end = bytes
            .iter()
            .rposition(|byte| !crate::scanners::is_ascii_whitespace(*byte))
            .map_or(leading, |position| position + 1)
            .max(leading);
        assert_eq!(fence_info_bounds(&bytes), leading..end);
    }
}
