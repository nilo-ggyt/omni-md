// Copyright 2015 Google Inc. All rights reserved.
// 合成测试以真实 firstpass 与保留的原 LineStart 为 oracle；完整 MIT 条款见 LICENSE。

use super::*;
use crate::block_context::ContainerRole;
use crate::global_frontier::{
    GlobalFact, GlobalFrontier, GlobalFrontierLimits, GlobalStep, StructuralSpineLimits,
};
use crate::line_start_stream::oracle::OracleLineStart;
use crate::parse::{Item, ItemBody};
use crate::tree::{Tree, TreeIndex};
use crate::{Options, Parser};

fn dialect() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_MATH
        | Options::ENABLE_SUPERSCRIPT
}

fn authority() -> GlobalSourceAuthority {
    GlobalSourceAuthority {
        current_revision: 71,
        cancelled: false,
    }
}

fn blocks(source: &str, width: usize) -> Vec<VerifiedGlobalInlineBlock> {
    // 票据只能来自真实 GlobalFrontier，测试也不以手工私有字段制造来源权限。
    let mut producer = GlobalFrontier::new(
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
    .unwrap();
    let mut result = Vec::new();
    for _ in 0..source.len().saturating_mul(100) + 10000 {
        let start = producer.next_byte();
        let end = start.saturating_add(width).min(source.len());
        match producer
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority(),
                4096,
            )
            .unwrap()
        {
            GlobalStep::Fact {
                fact: GlobalFact::InlineBlock(block),
                ..
            } => result.push(block),
            GlobalStep::Complete { .. } => return result,
            _ => (), // 其余结构事实不授予本 producer 的普通词法来源权限。
        }
    }
    panic!("真实 GlobalFrontier 没有结束");
}

fn limits(source: &str) -> BlockOriginLimits {
    BlockOriginLimits {
        max_byte_work: source.len().saturating_mul(1000) + 1_000_000,
        max_transitions: source.len().saturating_mul(10000) + 1_000_000,
    }
}

#[test]
/// 两遍原 Parser 的真实 Code/Math 规范化和跨行 Link 值必须一致，样式位相同本身不足以证明功能。
fn bounded_scoped_parser_preserves_original_inline_values_across_physical_prefixes() {
    for source in [
        "> **中文** ` 代码\n> \t续行😀 ` 和 $甲\n> 乙$\n",
        "> - [跨行链接](\n>   https://example.com/across\n>   \"实际 &amp; 标题\") 与 ` a\r\n>   \tb `\r\n",
        "- > [链接](https://example.com/a \"标题\") 与 ` \n  > \t `\n",
        "> 标题 ` 多行\n> 代码😀 ` 和 $甲\n> 乙$\n> ===\n",
    ] {
        let block = blocks(source, 1).into_iter().find(|block| block.scope_depth() > 1).unwrap();
        let physical = block.physical_range();
        let (origins, seed) = block.into_origins_and_semantic_seed(limits(source)).unwrap();
        drop(origins); // 只保这次真实闭合 seed；不存在另一个运行中的来源生产者或手造内部状态。
        let mut context = seed.bounded_paragraph_context(super::super::InlinePlainLimits {
            max_byte_work: limits(source).max_byte_work,
            max_transitions: limits(source).max_transitions,
        }).unwrap().unwrap();
        let raw = &source[physical.clone()];
        for _ in 0..source.len() * 100 + 10000 {
            context.advance(raw, authority(), 4096).unwrap();
            if context.is_complete() { break; }
            assert!(!context.is_unsupported());
        }
        assert!(context.is_complete());
        let interesting = |event: &crate::Event<'_>| matches!(event,
            crate::Event::Code(_) | crate::Event::InlineMath(_) | crate::Event::DisplayMath(_)
            | crate::Event::Start(crate::Tag::Link { .. }));
        let expected = Parser::new_ext(source, dialect()).into_offset_iter()
            .filter(|(event, _)| interesting(event))
            .map(|(event, range)| (event.into_static(), range)).collect::<Vec<_>>();
        let actual = Parser::<crate::DefaultBrokenLinkCallback>::new_with_inline_block_seed(raw, &context, None)
            .unwrap().into_offset_iter().filter(|(event, _)| interesting(event))
            .map(|(event, range)| (event.into_static(), range.start + physical.start..range.end + physical.start))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "source={source:?}");
    }
}

fn collect(source: &str, cursor: &mut BlockOriginCursor, width: usize) -> Vec<BlockOriginLine> {
    let mut result = Vec::new();
    let mut total = GlobalWork::default();
    for _ in 0..source.len().saturating_mul(100) + 10000 {
        let start = cursor.next_byte();
        let end = start.saturating_add(width).min(source.len());
        let step = cursor
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority(),
                4096,
            )
            .unwrap_or_else(|error| panic!("{error:?} source={source:?} cursor={cursor:?}"));
        let work = match &step {
            BlockOriginStep::Progress { work, .. }
            | BlockOriginStep::NeedInput { work, .. }
            | BlockOriginStep::Line { work, .. }
            | BlockOriginStep::Complete { work } => *work,
        };
        assert!(work.byte_reads <= 4096 && work.transitions <= 4096);
        total.byte_reads += work.byte_reads;
        total.transitions += work.transitions;
        assert_eq!(cursor.total_work(), total);
        match step {
            BlockOriginStep::Line { line, .. } => result.push(line),
            BlockOriginStep::Complete { .. } => return result,
            _ => (), // NeedInput 没有完成行，更不代表 source EOF。
        }
    }
    panic!("逐物理行来源没有结束");
}

fn role(body: ItemBody) -> ContainerRole {
    match body {
        ItemBody::BlockQuote(..) => ContainerRole::Quote,
        ItemBody::ListItem(indent) => ContainerRole::Item(indent),
        ItemBody::DefinitionListDefinition(indent) => ContainerRole::Definition(indent),
        ItemBody::FootnoteDefinition(..) => ContainerRole::Footnote,
        _ => ContainerRole::Other,
    }
}

fn find_roles(
    tree: &Tree<Item>,
    mut next: Option<TreeIndex>,
    start: usize,
    kind: InlineBlockKind,
    path: &mut Vec<ContainerRole>,
) -> Option<Vec<ContainerRole>> {
    // 完整 AST、递归和 Vec 仅属于这些有限合成 oracle；不进入产品或深 scope 工作负载。
    while let Some(index) = next {
        let node = &tree[index];
        path.push(role(node.item.body));
        let same_kind = match (node.item.body, kind) {
            (ItemBody::Paragraph | ItemBody::TightParagraph, InlineBlockKind::Paragraph) => true,
            (ItemBody::Heading(level, _), InlineBlockKind::SetextHeading(expected)) => {
                level == expected
            }
            _ => false,
        };
        if node.item.start == start && same_kind {
            return Some(path.clone());
        }
        if let Some(found) = find_roles(tree, node.child, start, kind, path) {
            return Some(found);
        }
        path.pop();
        next = node.next;
    }
    None
}

fn old_prefix<'a>(bytes: &'a [u8], roles: &[ContainerRole]) -> (OracleLineStart<'a>, usize) {
    let mut line = OracleLineStart::new(bytes);
    let mut matched = 0;
    for &role in roles {
        // 保留原 firstpass scan_containers 的 save/restore 与失败短路，
        // 期望值不调用新 ContainerPrefix 或它导出的 metadata。
        let save = line.clone();
        let accepted = match role {
            ContainerRole::Quote => {
                line.scan_space(3);
                line.scan_blockquote_marker()
            }
            ContainerRole::Item(indent) | ContainerRole::Definition(indent) => {
                line.scan_space(indent) || line.is_at_eol()
            }
            ContainerRole::Footnote => line.scan_space(4) || line.is_at_eol(),
            ContainerRole::Other => true,
        };
        if !accepted {
            line = save;
            break;
        }
        matched += 1;
    }
    (line, matched)
}

fn compare(source: &str, width: usize) {
    let expected = crate::firstpass::capture_closed_inline_blocks(source, dialect());
    let mut tree = crate::firstpass::run_first_pass(source, dialect()).0;
    tree.reset();
    let actual = blocks(source, width);
    assert_eq!(actual.len(), expected.len());
    for (block, (kind, extent, lexical)) in actual.into_iter().zip(expected) {
        assert_eq!(
            (block.kind(), block.source_range(), block.lexical_range()),
            (kind, extent, lexical.clone())
        );
        let roles = find_roles(&tree, tree.cur(), lexical.start, kind, &mut Vec::new()).unwrap();
        let mut cursor = block.into_line_origins(limits(source)).unwrap();
        let rows = collect(source, &mut cursor, width);
        let mut physical = source.as_bytes()[..lexical.start]
            .iter()
            .rposition(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(0, |position| position + 1);
        for (ordinal, actual) in rows.iter().enumerate() {
            let (content, container, final_state, matched) = if ordinal == 0 {
                // 首行来源以真实 firstpass 完成时的 lexical.start 为准；其 tab 状态另有原 opening 对照。
                (lexical.start, None, None, roles.len())
            } else {
                let (mut old, matched) = old_prefix(&source.as_bytes()[physical..], &roles);
                let container = old.state();
                old.scan_space(4);
                old.scan_all_space();
                (
                    physical + old.bytes_scanned(),
                    Some(container),
                    Some(old.state()),
                    matched,
                )
            };
            let content_end = source.as_bytes()[content..]
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .map_or(source.len(), |length| content + length);
            let eol_end = match source.as_bytes().get(content_end..) {
                Some([b'\r', b'\n', ..]) => content_end + 2,
                Some([b'\r' | b'\n', ..]) => content_end + 1,
                _ => content_end,
            };
            assert_eq!(
                actual.physical_range(),
                physical..eol_end,
                "source={source:?}"
            );
            assert_eq!(
                actual.prefix_range(),
                physical..content,
                "source={source:?}"
            );
            assert_eq!(
                actual.content_range(),
                content..content_end,
                "source={source:?}"
            );
            assert_eq!(
                actual.eol_range(),
                content_end..eol_end,
                "source={source:?}"
            );
            assert_eq!(actual.line_ordinal(), ordinal);
            assert_eq!(actual.matched_depth(), matched);
            assert_eq!(actual.total_depth(), roles.len());
            assert_eq!(actual.is_terminal(), eol_end == lexical.end);
            assert_eq!(actual.block_kind(), super::BlockOriginKind::Inline(kind));
            assert_eq!(
                actual.kind(),
                if ordinal == 0 {
                    BlockOriginLineKind::First
                } else {
                    BlockOriginLineKind::Continuation
                }
            );
            if let Some(state) = container {
                assert_eq!(
                    actual.container_state(),
                    BlockOriginState::capture(physical, state)
                );
            }
            if let Some(state) = final_state {
                assert_eq!(
                    actual.content_state(),
                    BlockOriginState::capture(physical, state)
                );
            }
            physical = eol_end;
        }
        assert_eq!(physical, lexical.end, "Setext underline 不得进入正文行来源");
        assert!(rows.last().unwrap().is_terminal());
        assert_eq!(cursor.buffered_source_capacity(), 0);
    }
    // 原 public Parser 必须能完整完成同一输入；终行 separator 不是另外一个最终 break。
    let _: Vec<_> = Parser::new_ext(source, dialect())
        .into_offset_iter()
        .collect();
}

#[test]
fn physical_origins_match_original_firstpass_and_line_start_at_every_small_frame() {
    let sources = [
        "plain",
        "one\ntwo\n\nnext",
        "中😀\r\n第二行\r第三行\n\nnext",
        "> first\r\n> second\r\nlazy 中文😀\r\n\r\n> next\r\n",
        "- first\n  second\n lazy\n\n- next\n",
        "> - first\r\n> \tsecond\r\n> lazy\r\n\r\n",
        "- [x] task\r\n  中文😀\r\n  tail",
        " >\t> first\r\n >\t>\tsecond\r\n lazy\r\n",
        "a\\\r\nb  \r\nc\\\r\n\r\nnext",
        "setext\\\r\n---\r\nnext\r\n",
        "> setext  \r\n> ===\r\n\r\n",
        "> - [ ] task\n>   second\n",
        "[^f]: 中😀\r\n    continued\r\n\r\nnext",
        "> [r]: /url\r\nlazy paragraph\r\n\r\n",
    ];
    for source in sources {
        for width in [1, 2, 7, 4096] {
            compare(source, width);
        }
    }
}

#[test]
fn first_line_preserves_original_opening_tab_state_without_continuation_rescan() {
    for source in [
        ">\t> first\r\n>\t> second\r\n",
        " >\t> first\r\n >\t> second\r\n",
    ] {
        let mut old = OracleLineStart::new(source.as_bytes());
        // 原 opening 的 quote 扫描在 marker 前最多三格、marker 后消费一格；
        // 外层空格与部分 tab 因此要原样保留，不能用 content byte 位置反推余宽。
        old.scan_space(3);
        assert!(old.scan_blockquote_marker());
        old.scan_space(3);
        assert!(old.scan_blockquote_marker());
        old.scan_space(4);
        let expected = old.state();
        let block = blocks(source, 1).remove(0);
        assert_eq!(block.lexical_range().start, old.bytes_scanned());
        let mut cursor = block.into_line_origins(limits(source)).unwrap();
        let rows = collect(source, &mut cursor, 1);
        assert_eq!(
            rows[0].container_state(),
            BlockOriginState::capture(0, expected)
        );
        assert_eq!(
            rows[0].content_state(),
            BlockOriginState::capture(0, expected)
        );
    }
}

#[test]
fn false_frame_eof_is_rejected_and_missing_real_eof_cannot_close_the_last_line() {
    let source = "中😀\r\nlast";
    let mut cursor = blocks(source, 1)
        .remove(0)
        .into_line_origins(limits(source))
        .unwrap();
    let start = cursor.next_byte();
    assert_eq!(
        cursor
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..start + 1],
                    real_eof: true
                },
                authority(),
                4096,
            )
            .unwrap_err(),
        GlobalFrontierError::Frontier(crate::block_frontier::FrontierError::InvalidFrame)
    );
    assert_eq!(cursor.next_byte(), start);
    let mut seen = Vec::new();
    for _ in 0..100 {
        let start = cursor.next_byte();
        let end = (start + 1).min(source.len());
        let step = cursor
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: false,
                },
                authority(),
                4096,
            )
            .unwrap();
        match step {
            BlockOriginStep::Line { line, .. } => seen.push(line),
            BlockOriginStep::NeedInput { next_byte, .. } if next_byte == source.len() => break,
            BlockOriginStep::Complete { .. } => panic!("frame 尾不能关闭无 EOL 末行"),
            _ => (),
        }
    }
    assert_eq!(seen.len(), 1);
    let step = cursor
        .advance(
            SourceFrame {
                source_revision: 71,
                byte_start: source.len(),
                bytes: &[],
                real_eof: true,
            },
            authority(),
            4096,
        )
        .unwrap();
    assert!(
        matches!(step, BlockOriginStep::Line { line, .. } if line.is_terminal() && line.eol_range().is_empty())
    );
}

#[test]
fn cancellation_revision_and_cumulative_fuel_failures_are_sticky() {
    let source = "one\r\ntwo\r\nthree";
    for rejected in [
        GlobalFrontierError::Cancelled,
        GlobalFrontierError::SourceSuperseded,
    ] {
        let mut cursor = blocks(source, 7)
            .remove(0)
            .into_line_origins(limits(source))
            .unwrap();
        let wrong = GlobalSourceAuthority {
            current_revision: if rejected == GlobalFrontierError::SourceSuperseded {
                72
            } else {
                71
            },
            cancelled: rejected == GlobalFrontierError::Cancelled,
        };
        let start = cursor.next_byte();
        assert_eq!(
            cursor
                .advance(
                    SourceFrame {
                        source_revision: 71,
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
                        source_revision: 71,
                        byte_start: start,
                        bytes: &source.as_bytes()[start..],
                        real_eof: true
                    },
                    authority(),
                    4096
                )
                .unwrap_err(),
            rejected
        );
    }
    for restricted in [
        BlockOriginLimits {
            max_byte_work: 3,
            max_transitions: 1_000_000,
        },
        BlockOriginLimits {
            max_byte_work: 1_000_000,
            max_transitions: 4096,
        },
    ] {
        let mut cursor = blocks(source, 7)
            .remove(0)
            .into_line_origins(restricted)
            .unwrap();
        let mut failed = false;
        for _ in 0..100 {
            let start = cursor.next_byte();
            let end = (start + 2).min(source.len());
            match cursor.advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority(),
                4096,
            ) {
                Err(GlobalFrontierError::WorkFuel) => {
                    failed = true;
                    break;
                }
                Ok(BlockOriginStep::Complete { .. }) => panic!("不能逐行重新发放总 fuel"),
                Ok(_) => (),
                Err(other) => panic!("意外失败 {other:?}"),
            }
        }
        assert!(failed);
        assert_eq!(
            cursor
                .advance(
                    SourceFrame {
                        source_revision: 71,
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
}

fn cursor_at_child_phase(source: &str, target: Phase) -> BlockOriginCursor {
    // 从真实闭合 Global 票据逐字节驱动到续行阶段；不手工构造来源权限或内部扫描器。
    let mut cursor = blocks(source, 1)
        .remove(0)
        .into_line_origins(limits(source))
        .unwrap();
    for _ in 0..10000 {
        if matches!(
            (cursor.phase, target),
            (Phase::Containers, Phase::Containers)
                | (Phase::Space, Phase::Space)
                | (Phase::AllSpace, Phase::AllSpace)
        ) {
            return cursor;
        }
        let start = cursor.next_byte();
        let end = (start + 1).min(source.len());
        let step = cursor
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: start,
                    bytes: &source.as_bytes()[start..end],
                    real_eof: end == source.len(),
                },
                authority(),
                4096,
            )
            .unwrap();
        assert!(
            !matches!(step, BlockOriginStep::Complete { .. }),
            "真实多行夹具应先经过目标续行阶段"
        );
    }
    panic!("真实来源没有到达目标子游标阶段");
}

#[test]
fn cancelled_and_superseded_child_phases_keep_position_and_debug_safe_without_revival() {
    let source = "> first\r\n> \tsecond\r\n> third\r\n";
    for target in [Phase::Containers, Phase::Space, Phase::AllSpace] {
        for rejected in [
            GlobalFrontierError::Cancelled,
            GlobalFrontierError::SourceSuperseded,
        ] {
            let mut cursor = cursor_at_child_phase(source, target);
            let start = cursor.next_byte();
            let total = cursor.total_work();
            let wrong = GlobalSourceAuthority {
                current_revision: if rejected == GlobalFrontierError::SourceSuperseded {
                    72
                } else {
                    71
                },
                cancelled: rejected == GlobalFrontierError::Cancelled,
            };
            assert_eq!(
                cursor
                    .advance(
                        SourceFrame {
                            source_revision: 71,
                            byte_start: start,
                            bytes: &source.as_bytes()[start..start + 1],
                            real_eof: false,
                        },
                        wrong,
                        4096
                    )
                    .unwrap_err(),
                rejected
            );
            assert!(matches!(cursor.phase, Phase::Failed));
            assert!(cursor.prefix.is_none() && cursor.operation.is_none());
            assert_eq!(cursor.next_byte(), start);
            assert!(
                !format!("{cursor:?}").is_empty(),
                "失败后的 Debug 也不能读取已释放子对象"
            );
            // 直接把失败后的 getter 用于下一 frame；正确 authority 和更大预算仍只能得到原首因。
            assert_eq!(
                cursor
                    .advance(
                        SourceFrame {
                            source_revision: 71,
                            byte_start: cursor.next_byte(),
                            bytes: &[],
                            real_eof: false,
                        },
                        authority(),
                        65536
                    )
                    .unwrap_err(),
                rejected
            );
            assert_eq!(cursor.total_work(), total);
        }
    }
}

#[test]
fn failure_after_detaching_a_child_keeps_a_safe_tombstone_instead_of_complete() {
    let source = "> first\r\n> \tsecond\r\n> third\r\n";
    for target in [Phase::Containers, Phase::Space, Phase::AllSpace] {
        let mut cursor = cursor_at_child_phase(source, target);
        let fallback = cursor.next;
        // 有意在真实票据上注入“资源已移走、后继 fallible 转换失败”的时序；不制造任何成功来源。
        // 这与 Space 完成后先清 operation、再 checked_add/start_space 失败的清理入口相同。
        cursor.prefix = None;
        cursor.operation = None;
        assert_eq!(
            cursor.fail(GlobalFrontierError::WorkFuel),
            GlobalFrontierError::WorkFuel
        );
        assert!(matches!(cursor.phase, Phase::Failed));
        assert_eq!(cursor.next_byte(), fallback);
        assert!(cursor.next_byte() <= source.len());
        assert!(!format!("{cursor:?}").is_empty());
        assert_eq!(
            cursor
                .advance(
                    SourceFrame {
                        source_revision: 71,
                        byte_start: cursor.next_byte(),
                        bytes: &[],
                        real_eof: false,
                    },
                    authority(),
                    65536
                )
                .unwrap_err(),
            GlobalFrontierError::WorkFuel
        );
    }
}

#[test]
fn deep_scope_keeps_original_shared_role_ledger_and_releases_page_claims_on_drop() {
    let source = format!(
        "{}first\r\n{}second\r\n",
        "> ".repeat(2048),
        "> ".repeat(2048)
    );
    let block = blocks(&source, 4096).remove(0);
    let mut observer = block.spine.clone();
    while !observer.is_empty() {
        // 仅留下原共享账本的观察者，不持任何页，才能实际验证 cursor Drop 归还末个页 claim。
        observer.pop().unwrap();
    }
    let before = observer.payload_counts();
    assert_eq!(block.scope_depth(), 2049);
    let mut cursor = block.into_line_origins(limits(&source)).unwrap();
    let rows = collect(&source, &mut cursor, 7);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].matched_depth(), 2049);
    assert_eq!(observer.payload_counts().0, before.0);
    assert_eq!(observer.payload_counts().1, before.1);
    assert!(
        observer.payload_counts().2 > before.2,
        "续行必须支付原 producer 的共享 role fuel"
    );
    drop(cursor);
    assert_eq!(observer.payload_counts().0, 0);
    assert_eq!(
        observer.payload_counts().1,
        before.1,
        "Drop 不归还历史 allocation fuel"
    );
}

#[test]
fn long_single_line_and_many_rows_use_zero_source_buffer_and_preserve_absolute_bytes() {
    for source in [
        "中😀".repeat(10000),
        format!("{}last", "中😀\r\n".repeat(1000)),
    ] {
        for width in [1, 2, 7, 4096] {
            let block = blocks(&source, 4096).remove(0);
            let mut cursor = block.into_line_origins(limits(&source)).unwrap();
            let rows = collect(&source, &mut cursor, width);
            assert_eq!(rows.first().unwrap().physical_range().start, 0);
            assert_eq!(rows.last().unwrap().physical_range().end, source.len());
            assert_eq!(cursor.buffered_source_capacity(), 0);
        }
    }
}

#[test]
fn dense_short_lines_pay_actual_workspace_events_with_original_core_fuel_at_minimum_step() {
    let source = (0..300)
        .map(|line| format!("x{line}\n"))
        .collect::<String>();
    let expected = crate::firstpass::capture_closed_inline_blocks(&source, dialect());
    assert_eq!(expected.len(), 1);
    for width in [1, 2, 7, 4096] {
        for budget in [2048, 4096] {
            // 仍使用 Core 原300短行与原累计上限；真实票据来自同一 Global，不放宽 fuel。
            let block = blocks(&source, width).remove(0);
            assert_eq!(block.lexical_range(), expected[0].2);
            let observer = block.spine.clone();
            let mut cursor = block
                .into_line_origins(BlockOriginLimits {
                    max_byte_work: source.len() * 256 + 1024 * 1024,
                    max_transitions: source.len() * 2048 + 1024 * 1024,
                })
                .unwrap();
            let mut total = GlobalWork::default();
            let mut physical = 0;
            let mut rows = 0;
            let mut workspace_events = 0;
            let mut saw_scalar_only = false;
            let mut completed = false;
            for _ in 0..source.len() * 100 + 10000 {
                let phase = cursor.phase;
                let prefix_before = cursor.prefix.is_some();
                let role_before = observer.work_used();
                let start = cursor.next_byte();
                let end = (start + width).min(source.len());
                let step = cursor
                    .advance(
                        SourceFrame {
                            source_revision: 71,
                            byte_start: start,
                            bytes: &source.as_bytes()[start..end],
                            real_eof: end == source.len(),
                        },
                        authority(),
                        budget,
                    )
                    .unwrap();
                let work = match &step {
                    BlockOriginStep::Progress { work, .. }
                    | BlockOriginStep::NeedInput { work, .. }
                    | BlockOriginStep::Line { work, .. }
                    | BlockOriginStep::Complete { work } => *work,
                };
                assert!(
                    work.byte_reads <= budget && work.transitions <= budget,
                    "最小2048必须先准入真实工作区与控制费的最坏组合"
                );
                let workspace = if prefix_before != cursor.prefix.is_some() {
                    workspace_events += 1;
                    PREFIX_WORKSPACE_WORK
                } else {
                    0
                };
                let roles = observer.work_used() - role_before;
                if !matches!(phase, Phase::Complete) {
                    assert!(
                        work.transitions >= CONTROL_WORK + workspace + roles,
                        "共享原 role 增量与真实 workspace 事件均进入同一步总账"
                    );
                }
                if matches!(phase, Phase::First) {
                    // 首行只接管原 opening 状态，不能为没有创建的 continuation 工作区收1024。
                    assert_eq!(
                        work,
                        GlobalWork {
                            byte_reads: 0,
                            transitions: CONTROL_WORK
                        }
                    );
                    saw_scalar_only = true;
                }
                if matches!(phase, Phase::BeginContinuation) {
                    // 创建与释放分属不同真实阶段，本步只付一次工作区费且不重读正文。
                    assert_eq!(workspace, PREFIX_WORKSPACE_WORK);
                    assert_eq!(
                        work,
                        GlobalWork {
                            byte_reads: 0,
                            transitions: CONTROL_WORK + PREFIX_WORKSPACE_WORK
                        }
                    );
                }
                total.byte_reads += work.byte_reads;
                total.transitions += work.transitions;
                assert_eq!(cursor.total_work(), total);
                match step {
                    BlockOriginStep::Line { line, .. } => {
                        // 独立物理字节 oracle 不调用新来源输出计算期望边界，也不保整篇行集合。
                        let content_end = physical
                            + source.as_bytes()[physical..]
                                .iter()
                                .position(|byte| *byte == b'\n')
                                .unwrap();
                        let eol_end = content_end + 1;
                        assert_eq!(line.physical_range(), physical..eol_end);
                        assert_eq!(line.prefix_range(), physical..physical);
                        assert_eq!(line.content_range(), physical..content_end);
                        assert_eq!(line.eol_range(), content_end..eol_end);
                        assert_eq!(line.line_ordinal(), rows);
                        assert_eq!(line.is_terminal(), eol_end == source.len());
                        physical = eol_end;
                        rows += 1;
                    }
                    BlockOriginStep::Complete { .. } => {
                        completed = true;
                        break;
                    }
                    _ => (), // 未闭合的分帧/控制阶段没有行发布权限。
                }
            }
            assert!(completed && saw_scalar_only);
            assert_eq!((rows, physical), (300, source.len()));
            assert_eq!(
                workspace_events,
                2 * (rows - 1),
                "每条真实续行恰好创建并释放一次工作区，首行无额外opening重扫"
            );
            assert_eq!(cursor.buffered_source_capacity(), 0);
        }
    }
}
