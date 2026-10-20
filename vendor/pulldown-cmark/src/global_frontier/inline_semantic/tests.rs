// Copyright 2015 Google Inc. All rights reserved.
// 真实 Global 票据与完整 pinned Parser 是独立 oracle；完整 MIT 条款见 LICENSE。

use super::*;
use crate::global_frontier::{
    BlockOriginLimits, GlobalFact, GlobalFrontier, GlobalFrontierError, GlobalFrontierLimits,
    GlobalStep, StructuralSpineLimits,
};
use crate::{Event, Parser, Tag};

fn authority() -> GlobalSourceAuthority {
    GlobalSourceAuthority {
        current_revision: 41,
        cancelled: false,
    }
}
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_SUPERSCRIPT
}

#[test]
/// 必须由原 Root Fact 真正进入 code 路径；HTML、escape、失败 opener 重放均与完整 Parser 独立对照。
fn verified_code_spans_match_original_parser_at_split_frames_without_history() {
    for source in [
        "before `中文😀` after",
        r"before \``中文😀` after",
        "before ` x ` after",
        "before `   ` after",
        "pre <span title=\"`fake`\">x</span> `real`",
        "pre <!-- `fake` --> `real`",
        "`foo``bar``",
        "pre ` 中\r\n😀 ` after\r\n",
        "<u>before `</u>` true close</u>",
    ] {
        let expected: Vec<_> = Parser::new_ext(source, options())
            .into_offset_iter()
            .filter_map(|(event, range)| matches!(event, Event::Code(_)).then_some(range))
            .collect();
        for width in [1, 2, 7, 4096] {
            for budget in [2048, 4096] {
                let seed = seed(source, options());
                let mut cursor = seed
                    .code_cursor(seed.source_range().end, limits(source))
                    .unwrap()
                    .expect("原 Root Paragraph self 为唯一 scope，不得把合法入口误判 NonRoot");
                let mut actual = Vec::new();
                let mut finished = false;
                for _ in 0..source.len() * 100 + 10000 {
                    let start = cursor.next_byte();
                    let end = (start + width).min(source.len());
                    let step = cursor
                        .advance(
                            SourceFrame {
                                source_revision: 41,
                                byte_start: start,
                                bytes: &source.as_bytes()[start..end],
                                real_eof: end == source.len(),
                            },
                            authority(),
                            budget,
                        )
                        .unwrap();
                    let measured = match &step {
                        InlineCodeStep::Progress { work }
                        | InlineCodeStep::NeedInput { work }
                        | InlineCodeStep::Span { work, .. }
                        | InlineCodeStep::Prefix { work, .. }
                        | InlineCodeStep::Emphasis { work, .. }
                        | InlineCodeStep::Link { work, .. }
                        | InlineCodeStep::Storage { work, .. }
                        | InlineCodeStep::Reference { work, .. }
                        | InlineCodeStep::LinkEnd { work, .. }
                        | InlineCodeStep::Proven { work, .. }
                        | InlineCodeStep::NeedsGeneralInline { work } => *work,
                    };
                    assert!(measured.byte_reads + measured.transitions <= budget);
                    match step {
                        InlineCodeStep::Span { proof, .. } => {
                            assert_eq!(proof.source_revision(), 41);
                            assert_eq!(proof.source_end(), source.len());
                            let body = &source[proof.raw_body_range()];
                            let normalized = body.replace("\r\n", " ").replace(['\r', '\n'], " ");
                            let trim = normalized.starts_with(' ')
                                && normalized.ends_with(' ')
                                && !normalized.bytes().all(|byte| byte == b' ');
                            assert_eq!(proof.trims_boundary_spaces(), trim);
                            actual.push(proof.source_range());
                        }
                        InlineCodeStep::Proven {
                            requires_general_styles,
                            ..
                        } => {
                            assert!(!requires_general_styles);
                            finished = true;
                            break;
                        }
                        InlineCodeStep::NeedsGeneralInline { .. } => {
                            panic!("真实 plain/code/原 HTML 组合不应丢失 code 优先级: {source:?}")
                        }
                        _ => {}
                    }
                }
                assert!(
                    finished,
                    "游标必须在真实块末端完成，不能把短 frame 当作 EOF"
                );
                assert_eq!(
                    actual, expected,
                    "source={source:?} width={width} budget={budget}"
                );
            }
        }
    }
}
fn limits(source: &str) -> InlinePlainLimits {
    InlinePlainLimits {
        max_byte_work: source.len() * 256 + 1024 * 1024,
        max_transitions: source.len() * 2048 + 1024 * 1024,
    }
}

fn seed(source: &str, options: Options) -> VerifiedInlineSemanticSeed {
    source_pair(source, options).1
}

fn source_pair(
    source: &str,
    options: Options,
) -> (
    crate::global_frontier::BlockOriginCursor,
    VerifiedInlineSemanticSeed,
) {
    // 不手造权限：只有真实 producer 的已闭合 Fact 可以进入配对 factory。
    let mut global = GlobalFrontier::new(
        41,
        source.len(),
        options,
        GlobalFrontierLimits {
            spine: StructuralSpineLimits {
                max_depth: 16_384,
                max_live_page_bytes: 4 * 1024 * 1024,
                max_page_allocations: 1_000_000,
                max_role_work: 200_000_000,
            },
            max_byte_work: limits(source).max_byte_work,
            max_transitions: limits(source).max_transitions,
        },
    )
    .unwrap();
    for _ in 0..source.len() * 100 + 10000 {
        let start = global.next_byte();
        let end = (start + 7).min(source.len());
        if let GlobalStep::Fact {
            fact: GlobalFact::InlineBlock(block),
            ..
        } = global
            .advance(
                SourceFrame {
                    source_revision: 41,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority(),
                4096,
            )
            .unwrap()
        {
            let (origins, seed) = block
                .into_origins_and_semantic_seed(BlockOriginLimits {
                    max_byte_work: limits(source).max_byte_work,
                    max_transitions: limits(source).max_transitions,
                })
                .unwrap();
            return (origins, seed); // 两者共享同一原 scope 页；没有建立第二份正文或另一 Global。
        }
    }
    panic!("真实 Global 未交付闭合 inline Fact");
}

fn finish(
    source: &str,
    cursor: &mut InlinePlainCursor,
    width: usize,
    budget: usize,
) -> InlinePlainStep {
    for _ in 0..source.len() * 100 + 10000 {
        let start = cursor.next_byte();
        let end = (start + width).min(source.len());
        let step = cursor
            .advance(
                SourceFrame {
                    source_revision: 41,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority(),
                budget,
            )
            .unwrap();
        let work = match &step {
            InlinePlainStep::Progress { work }
            | InlinePlainStep::NeedInput { work, .. }
            | InlinePlainStep::NeedsGeneralInline { work }
            | InlinePlainStep::NonRootScope { work }
            | InlinePlainStep::Proven { work, .. } => *work,
        };
        assert!(work.byte_reads <= budget && work.transitions <= budget);
        match step {
            InlinePlainStep::Progress { .. } | InlinePlainStep::NeedInput { .. } => (),
            _ => return step,
        }
    }
    panic!("有限 Plain 前缀未完成");
}

#[test]
fn true_root_plain_and_setext_prefixes_match_parser_and_terminal_bytes_at_small_frames() {
    for source in [
        "中文😀 plain",
        "plain \r\nnext  \r\n",
        "title  \r\n===\r\n",
        "title\r\n  ---  \r\n",
        "one\rsecond\nthird\r\n",
    ] {
        let expected = Parser::new_ext(source, options())
            .into_offset_iter()
            .find_map(|(event, range)| match event {
                Event::Start(Tag::Paragraph) => Some((InlineBlockKind::Paragraph, range)),
                Event::Start(Tag::Heading { level, .. }) => {
                    Some((InlineBlockKind::SetextHeading(level), range))
                }
                _ => None,
            })
            .unwrap();
        for width in [1, 2, 7, 4096] {
            for budget in [2048, 4096] {
                let seed = seed(source, options());
                assert_eq!(
                    seed.scope_depth(),
                    1,
                    "Root 含原 Paragraph self 一层，不能把depth0当Root"
                );
                assert_eq!((seed.kind(), seed.source_range()), expected);
                let lexical = seed.lexical_range();
                let mut cursor = seed
                    .plain_cursor(seed.source_range().end, limits(source))
                    .unwrap();
                let InlinePlainStep::Proven { proof, .. } =
                    finish(source, &mut cursor, width, budget)
                else {
                    panic!("无 enabled inline special 的真实 Root 应可证明");
                };
                assert_eq!((proof.kind(), proof.source_range()), expected);
                let terminal = proof.terminal().unwrap();
                let tail = &source.as_bytes()[..lexical.end];
                let mut content_end = tail.len();
                if tail.last() == Some(&b'\n') {
                    content_end -= 1;
                    if tail.get(content_end.wrapping_sub(1)) == Some(&b'\r') {
                        content_end -= 1;
                    }
                } else if tail.last() == Some(&b'\r') {
                    content_end -= 1;
                }
                let mut trimmed = content_end;
                while trimmed > 0 && crate::scanners::is_ascii_whitespace_no_nl(tail[trimmed - 1]) {
                    trimmed -= 1;
                }
                assert_eq!(
                    terminal,
                    InlinePlainTerminal {
                        content_end,
                        trimmed_text_end: trimmed,
                        eol_end: lexical.end
                    }
                );
                if matches!(proof.kind(), InlineBlockKind::SetextHeading(_)) {
                    let marker = proof.marker_range().unwrap();
                    assert!(marker.start >= lexical.end && marker.end == expected.1.end);
                    assert!(matches!(source.as_bytes()[marker.start], b'=' | b'-'));
                } else {
                    assert!(proof.marker_range().is_none());
                }
            }
        }
    }
}

#[test]
fn proven_prefix_is_not_changed_by_future_special_and_forward_reuses_original_cursor_fuel() {
    let source = "plain 中文😀\r\nnext **bold**\r\n";
    let end = source.find("next").unwrap();
    let seed = seed(source, options());
    let observer = seed.spine.clone();
    let before_roles = observer.work_used();
    let mut cursor = seed.plain_cursor(end, limits(source)).unwrap();
    let InlinePlainStep::Proven { proof, .. } = finish(source, &mut cursor, 1, 2048) else {
        panic!("后继特殊字符不能倒改此前无opener的纯文本前缀");
    };
    assert_eq!(proof.proven_end(), end);
    assert!(proof.terminal().is_none());
    assert!(
        observer.work_used() > before_roles,
        "Root 验证仍真实访问原共享 role ledger"
    );
    let before = cursor.total_work();
    let extend = cursor.extend_to(source.len()).unwrap();
    assert_eq!(
        cursor.total_work().transitions,
        before.transitions + extend.transitions
    );
    assert!(matches!(
        finish(source, &mut cursor, 2, 4096),
        InlinePlainStep::NeedsGeneralInline { .. }
    ));
    assert!(cursor.total_work().transitions > before.transitions);
    assert!(
        Parser::new_ext(source, options()).any(|event| matches!(event, Event::Start(Tag::Strong)))
    );
}

#[test]
fn nonroot_and_enabled_special_are_pending_and_authority_fuel_failures_remain_sticky() {
    for source in ["> quoted\r\n", "- item\n", "[^f]: foot\n"] {
        let seed = seed(source, options());
        let mut cursor = seed
            .plain_cursor(seed.source_range().end, limits(source))
            .unwrap();
        assert!(matches!(
            finish(source, &mut cursor, 1, 2048),
            InlinePlainStep::NonRootScope { .. }
        ));
    }
    for text in [
        "*a*", "a_b", "a&b", "a\\b", "a[b]", "a<b", "a!b", "a`b", "a~b", "a^b", "a$b", "a{b",
        "a}b", "a|b",
    ] {
        let source = format!("plain {text}\n");
        let seed = seed(&source, options());
        let mut cursor = seed
            .plain_cursor(seed.source_range().end, limits(&source))
            .unwrap();
        assert!(
            matches!(
                finish(&source, &mut cursor, 1, 2048),
                InlinePlainStep::NeedsGeneralInline { .. }
            ),
            "source={source}"
        );
    }
    let source = "plain text\r\n";
    for rejected in [
        GlobalFrontierError::Cancelled,
        GlobalFrontierError::SourceSuperseded,
    ] {
        let seed = seed(source, options());
        let mut cursor = seed.plain_cursor(source.len(), limits(source)).unwrap();
        let start = cursor.next_byte();
        let wrong = GlobalSourceAuthority {
            current_revision: if rejected == GlobalFrontierError::SourceSuperseded {
                42
            } else {
                41
            },
            cancelled: rejected == GlobalFrontierError::Cancelled,
        };
        assert_eq!(
            cursor
                .advance(
                    SourceFrame {
                        source_revision: 41,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..],
                        real_eof: true
                    },
                    wrong,
                    4096
                )
                .unwrap_err(),
            rejected
        );
        assert_eq!(
            cursor
                .advance(
                    SourceFrame {
                        source_revision: 41,
                        byte_start: cursor.next_byte(),
                        bytes: &[],
                        real_eof: false
                    },
                    authority(),
                    65536
                )
                .unwrap_err(),
            rejected
        );
        assert!(!format!("{cursor:?}").is_empty());
    }
    let seed = seed(source, options());
    let mut cursor = seed
        .plain_cursor(
            source.len(),
            InlinePlainLimits {
                max_byte_work: 1,
                max_transitions: 100000,
            },
        )
        .unwrap();
    let mut failed = false;
    for _ in 0..100 {
        let start = cursor.next_byte();
        let end = (start + 1).min(source.len());
        if let Err(error) = cursor.advance(
            SourceFrame {
                source_revision: 41,
                byte_start: start,
                bytes: &source.as_bytes()[start..end],
                real_eof: end == source.len(),
            },
            authority(),
            2048,
        ) {
            assert_eq!(error, GlobalFrontierError::WorkFuel);
            failed = true;
            break;
        }
    }
    assert!(failed);
    assert_eq!(
        cursor
            .advance(
                SourceFrame {
                    source_revision: 41,
                    byte_start: cursor.next_byte(),
                    bytes: &[],
                    real_eof: false
                },
                authority(),
                65536
            )
            .unwrap_err(),
        GlobalFrontierError::WorkFuel
    );
}

#[test]
/// 真 Origin 进入 code 的三阶段；1-byte frame 把 UTF8、CRLF、Tab 与前缀都切开，仍保原 canonical 匹配。
fn scoped_code_normal_probe_and_trim_share_actual_origin_at_split_frames() {
    for (source, trims) in [
        ("> ` 甲\r\n> 乙 ` 后文\r\n", vec![true]),
        ("> ` \n>   `\n", vec![false]),
        ("> ` \n> \t `\n", vec![true]),
        ("> - ` \r\n>   \t `\r\n", vec![true]),
        ("> - ` 甲\n>   乙 ` 与 ``后文``\n", vec![true, false]),
        ("1. `首行\r   末行` 后文\r", vec![false]),
        ("> `前行\n懒续行` 后文\n", vec![false]),
        (
            "> `未配对 **正文\n> 续行** 与 ``真实\n> 代码``\n",
            vec![false],
        ),
    ] {
        let expected: Vec<_> = Parser::new_ext(source, options())
            .into_offset_iter()
            .filter_map(|(event, range)| matches!(event, Event::Code(_)).then_some(range))
            .collect();
        assert_eq!(expected.len(), trims.len());
        for width in [1, 2, 7, 4096] {
            for budget in [2048, 4096] {
                let (mut origins, seed) = source_pair(source, options());
                let origin = loop {
                    let start = origins.next_byte();
                    let end = (start + width).min(source.len());
                    match origins
                        .advance(
                            SourceFrame {
                                source_revision: 41,
                                byte_start: start,
                                bytes: &source.as_bytes()[start..end],
                                real_eof: end == source.len(),
                            },
                            authority(),
                            budget,
                        )
                        .unwrap()
                    {
                        crate::global_frontier::BlockOriginStep::Line { line, .. } => break line,
                        crate::global_frontier::BlockOriginStep::Complete { .. } => {
                            panic!("真实段落须有原 Origin")
                        }
                        _ => {}
                    }
                };
                let mut cursor = seed
                    .emphasis_origin_cursor(&origin, seed.source_range().end, limits(source))
                    .unwrap()
                    .unwrap();
                drop(origins);
                let mut actual = Vec::new();
                let mut actual_trims = Vec::new();
                let mut finished = false;
                for _ in 0..source.len() * 1000 + 10000 {
                    let start = cursor.next_byte();
                    let end = (start + width).min(source.len());
                    let step = cursor.advance(SourceFrame {
                        source_revision: 41, byte_start: start, bytes: &source.as_bytes()[start..end], real_eof: end == source.len(),
                    }, authority(), budget).unwrap_or_else(|error| panic!("{error:?}: source={source:?} width={width} budget={budget} cursor={cursor:?}"));
                    let work = match &step {
                        InlineCodeStep::Progress { work }
                        | InlineCodeStep::NeedInput { work }
                        | InlineCodeStep::Span { work, .. }
                        | InlineCodeStep::Prefix { work, .. }
                        | InlineCodeStep::Emphasis { work, .. }
                        | InlineCodeStep::Link { work, .. }
                        | InlineCodeStep::Storage { work, .. }
                        | InlineCodeStep::Reference { work, .. }
                        | InlineCodeStep::LinkEnd { work, .. }
                        | InlineCodeStep::Proven { work, .. }
                        | InlineCodeStep::NeedsGeneralInline { work } => *work,
                    };
                    assert!(work.byte_reads + work.transitions <= budget);
                    match step {
                        InlineCodeStep::Span { proof, .. } => {
                            actual.push(proof.source_range());
                            actual_trims.push(proof.trims_boundary_spaces());
                        }
                        InlineCodeStep::Prefix { proof, .. } => {
                            let range = proof.source_range();
                            assert!(
                                range.end <= source.len()
                                    && source.is_char_boundary(range.start)
                                    && source.is_char_boundary(range.end)
                            );
                        }
                        InlineCodeStep::Proven {
                            requires_general_styles,
                            ..
                        } => {
                            assert!(!requires_general_styles);
                            finished = true;
                            break;
                        }
                        InlineCodeStep::NeedsGeneralInline { .. } => {
                            panic!("Code/强调与已证容器不能误降为未知：{source:?}")
                        }
                        _ => {}
                    }
                }
                assert!(finished, "source={source:?} width={width} budget={budget}");
                assert_eq!(
                    actual, expected,
                    "source={source:?} width={width} budget={budget}"
                );
                assert_eq!(
                    actual_trims, trims,
                    "真实容器前缀不能参与 code 首尾空白判断：{source:?}"
                );
            }
        }
    }
}

#[test]
/// 真实文件属于 Core，vendor 不造第二个 Vec store 来替测试签正面 proof。
/// 该层只验证公开暂停点仍保同源撤权；完整事件/目标/UTF8 差分在 Core 原真实 StackFile 路径完成。
fn streamed_links_suspend_for_original_storage_and_keep_source_revocation() {
    for source in [
        "[reference]",
        "![image](x)",
        "[outer [inner](x)](y)",
        "[x](<bad",
    ] {
        for revoked in [false, true] {
            let seed = seed(source, options());
            let mut cursor = seed
                .code_cursor(seed.source_range().end, limits(source))
                .unwrap()
                .unwrap();
            let request = loop {
                let at = cursor.next_byte();
                let end = (at + 1).min(source.len());
                let step = cursor
                    .advance(
                        SourceFrame {
                            source_revision: 41,
                            byte_start: at,
                            bytes: &source.as_bytes()[at..end],
                            real_eof: end == source.len(),
                        },
                        authority(),
                        2048,
                    )
                    .unwrap();
                match step {
                    InlineCodeStep::Storage { request, work } => {
                        assert!(work.byte_reads + work.transitions <= 2048);
                        break request;
                    }
                    InlineCodeStep::Progress { .. } | InlineCodeStep::NeedInput { .. } => {}
                    _ => panic!("未落真实括号记录前不得发布链接或局部 Ready"),
                }
            };
            // 不执行任何假 I/O。已暂停的同一请求也必须在移交前拒绝取消/换代，且首错不可恢复。
            let cancelled = GlobalSourceAuthority {
                current_revision: if revoked { 42 } else { 41 },
                cancelled: !revoked,
            };
            let result = cursor.accept_storage(
                &request,
                InlineLinkStorageStep::Progress {
                    work: GlobalWork::default(),
                },
                cancelled,
                2048,
            );
            if revoked {
                assert!(matches!(result, Err(GlobalFrontierError::SourceSuperseded)));
            } else {
                assert!(matches!(result, Err(GlobalFrontierError::Cancelled)));
            }
            let at = cursor.next_byte();
            let result = cursor.advance(
                SourceFrame {
                    source_revision: 41,
                    byte_start: at,
                    bytes: &[],
                    real_eof: false,
                },
                authority(),
                2048,
            );
            if revoked {
                assert!(matches!(result, Err(GlobalFrontierError::SourceSuperseded)));
            } else {
                assert!(matches!(result, Err(GlobalFrontierError::Cancelled)));
            }
        }
    }
}

#[test]
/// 普通 Parser 也消费共享禁用规则；显式固定语法结果保护 Image 内 Link 与祖先禁用的业务语义。
fn original_parser_keeps_nested_link_and_image_decisions() {
    for (source, expected) in [
        ("[a [b](inner)](outer)", vec![(false, "inner")]),
        (
            "![a [b](inner)](image)",
            vec![(true, "image"), (false, "inner")],
        ),
        (
            "[a ![b](image)](outer)",
            vec![(false, "outer"), (true, "image")],
        ),
        (
            "![a ![b](inner)](outer)",
            vec![(true, "outer"), (true, "inner")],
        ),
        ("[a [missing] b](outer)", vec![(false, "outer")]),
        ("[x][key]\n\n[key]: /resolved", vec![(false, "/resolved")]),
        (
            "[key][] [key]\n\n[key]: /resolved",
            vec![(false, "/resolved"), (false, "/resolved")],
        ),
    ] {
        let actual: Vec<_> = Parser::new_ext(source, options())
            .filter_map(|event| match event {
                Event::Start(Tag::Link { dest_url, .. }) => Some((false, dest_url.into_string())),
                Event::Start(Tag::Image { dest_url, .. }) => Some((true, dest_url.into_string())),
                _ => None,
            })
            .collect();
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(image, url)| (image, url.to_owned()))
            .collect();
        assert_eq!(actual, expected, "原 LinkStack 决策不能因外存适配改变");
    }
}
