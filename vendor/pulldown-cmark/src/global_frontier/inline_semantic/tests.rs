// Copyright 2015 Google Inc. All rights reserved.
// 真实 Global 票据与完整 pinned Parser 是独立 oracle；完整 MIT 条款见 LICENSE。

use super::*;
use crate::global_frontier::{
    GlobalFact, GlobalFrontier, GlobalFrontierLimits, GlobalStep, InlineOriginLimits,
    StructuralSpineLimits,
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
fn limits(source: &str) -> InlinePlainLimits {
    InlinePlainLimits {
        max_byte_work: source.len() * 256 + 1024 * 1024,
        max_transitions: source.len() * 2048 + 1024 * 1024,
    }
}

fn seed(source: &str, options: Options) -> VerifiedInlineSemanticSeed {
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
                .into_origins_and_semantic_seed(InlineOriginLimits {
                    max_byte_work: limits(source).max_byte_work,
                    max_transitions: limits(source).max_transitions,
                })
                .unwrap();
            drop(origins); // 留下的 seed 仍共享真实 scope 页；没有保第二份原始 source。
            return seed;
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
