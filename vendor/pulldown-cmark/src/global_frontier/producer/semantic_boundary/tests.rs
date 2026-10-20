// Copyright 2015 Google Inc. All rights reserved.
// 恢复后仍比较真实来源事实与独立顺序扫描；MIT条款见LICENSE。

use super::*;

#[test]
fn explicit_root_restart_discards_partial_origin_without_reissuing_fuel_or_losing_restore_obligation(
) {
    let source = "中文😀 first\n\n> quoted\n\n| H |\n| - |\n| b |\n";
    let mut global = frontier(source);
    // 真正处于原UTF8扫描中途也能放弃目标；重放从已知真实根0开始，而非伪造旧行边界。
    for _ in 0..3 {
        drop(step(&mut global, source, 1));
    }
    let before = global.total_work();
    global.restart_from_root(authority(), 4096).unwrap();
    assert_eq!(global.next_byte(), 0);
    assert_eq!(global.total_work().byte_reads, before.byte_reads);
    assert!(global.total_work().transitions > before.transitions);
    let actual = tail(&mut global, source, 3);
    let expected = tail(&mut frontier(source), source, 3);
    assert_eq!(
        actual.iter().map(signature).collect::<Vec<_>>(),
        expected.iter().map(signature).collect::<Vec<_>>()
    );
    global
        .retain_semantic_boundary(authority(), 4096)
        .unwrap()
        .unwrap();
    assert_eq!(
        global.restart_from_root(authority(), 4096),
        Err(GlobalFrontierError::InvalidFrame)
    );
    assert!(global.semantic_boundary_retained());
    global.restore_semantic_boundary(authority(), 4096).unwrap();
}

fn authority() -> GlobalSourceAuthority {
    GlobalSourceAuthority {
        current_revision: 71,
        cancelled: false,
    }
}

/// 固定短语料的真实前沿；没有正文快照、第二初始化预算或假造的块/表格种子。
fn frontier(source: &str) -> GlobalFrontier {
    GlobalFrontier::new(
        71,
        source.len(),
        Options::ENABLE_TABLES
            | Options::ENABLE_GFM
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
        GlobalFrontierLimits {
            spine: StructuralSpineLimits {
                max_depth: 16_384,
                max_live_page_bytes: 4 * 1024 * 1024,
                max_page_allocations: 1_000_000,
                max_role_work: 200_000_000,
            },
            max_byte_work: 10_000_000,
            max_transitions: 100_000_000,
        },
    )
    .unwrap()
}

/// 单字节切分也只交真正源尾EOF；工作归属取自原Global，不从输入长度推算消费量。
fn step(frontier: &mut GlobalFrontier, source: &str, width: usize) -> GlobalStep {
    let start = frontier.next_byte();
    let end = (start + width).min(source.len());
    let step = frontier
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
    let work = match &step {
        GlobalStep::Progress { work, .. }
        | GlobalStep::NeedInput { work, .. }
        | GlobalStep::Fact { work, .. }
        | GlobalStep::Complete { work } => work,
    };
    assert!(work.byte_reads <= 4096 && work.transitions <= 4096);
    step
}

/// 仅比较短合成样本的真实语义标量与scope；不把共享页的分配次数当语义输出。
fn signature(fact: &GlobalFact) -> String {
    match fact {
        GlobalFact::Table(table) => format!("table {table:?}"),
        GlobalFact::Row(row) => format!("row {row:?}"),
        GlobalFact::TableEnd(end) => format!(
            "end {} {} {} {} {:?} {} {} {:?}",
            end.header_byte_start(),
            end.columns(),
            end.byte_end(),
            end.utf16_end(),
            end.last_body_row_ordinal(),
            end.source_line_at_end(),
            end.ended_with_eol(),
            end.continuation_prefix_byte_range()
        ),
        GlobalFact::Reference(reference) => format!(
            "reference {:?} {:?}",
            reference.candidate(),
            (0..reference.spine.len())
                .map(|index| reference.spine.entry(index).unwrap())
                .collect::<Vec<_>>()
        ),
        GlobalFact::InlineBlock(block) => format!(
            "inline {:?} {:?} {:?} {:?} {} {:?} {:?}",
            block.kind(),
            block.source_range(),
            block.physical_range(),
            block.lexical_range(),
            block.first_state_origin,
            block.first_state,
            (0..block.spine.len())
                .map(|index| block.spine.entry(index).unwrap())
                .collect::<Vec<_>>()
        ),
        GlobalFact::LeafBlock(block) => format!(
            "leaf {:?} {:?} {} {} {:?} {:?}",
            block.kind(),
            block.source_range(),
            block.physical_start(),
            block.consumed_end(),
            block.details(),
            leaf_scope(block)
        ),
    }
}

/// Leaf权限仍通过它自己的真实游标读取；不为回归开放或伪造私有spine字段。
fn leaf_scope(block: &VerifiedGlobalLeafBlock) -> Vec<(usize, usize, CapturedScopeRole)> {
    let mut cursor = block.scope_cursor().unwrap();
    let mut roles = Vec::new();
    loop {
        match cursor.advance(authority(), 4096).unwrap() {
            CapturedScopeStep::Entry {
                depth_index,
                source_byte_start,
                role,
                ..
            } => {
                roles.push((depth_index, source_byte_start, role));
            }
            CapturedScopeStep::Progress { .. } => {} // 原游标的页构建完成后才可取得真实角色。
            CapturedScopeStep::Complete { .. } => return roles,
        }
    }
}

/// 只有有限测试语料允许保所有Fact来比较；产品前探逐个消费并丢弃，不建立历史目录。
fn tail(frontier: &mut GlobalFrontier, source: &str, width: usize) -> Vec<GlobalFact> {
    let mut facts = Vec::new();
    loop {
        match step(frontier, source, width) {
            GlobalStep::Fact { fact, .. } => facts.push(fact),
            GlobalStep::Complete { .. } => return facts,
            _ => {} // 非完成步只推进同一真实前沿，不把短frame或无Fact视作EOF。
        }
    }
}

#[test]
fn real_block_and_table_boundaries_replay_the_same_facts_and_keep_issued_scopes() {
    let nested = "> ".repeat(80);
    let deep = format!(
        "{nested}first\r\n{nested}\r\n{nested}```text\r\n{nested}<u>中😀</u>\r\n{nested}```\r\n\r\ntail\n"
    );
    let sources = [
        "# first\r\n\r\n    <u>中😀</u>\r\n\r\nlast\rnext\n",
        "- first\n\n  ## title\n\n  body\n\n- second\n",
        "| a | b |\r\n|---|---|\r\n| 中 |\r\n| 😀 | 2 |\r\n\r\n[label]: /url \"标题\"\r\n\r\nlast\n",
        "> [!NOTE]\n> first\n>\n> | a | b |\n> |---|---|\n> | one |\n\nlast\n",
        deep.as_str(),
    ];
    for source in sources {
        let expected = tail(&mut frontier(source), source, 7)
            .iter()
            .map(signature)
            .collect::<Vec<_>>();
        for width in [1, 3, 64] {
            // 每个真实Fact后均尝试；引用尾不是边界，须由原前沿继续解释，不能伪存Block。
            for checkpoint_after in 0..expected.len() {
                let mut global = frontier(source);
                let budget = global.spine.empty_shared();
                let mut issued = Vec::new();
                while issued.len() <= checkpoint_after {
                    if let GlobalStep::Fact { fact, .. } = step(&mut global, source, width) {
                        issued.push(fact);
                    }
                }
                let before = global.total_work();
                let Some(captured) = global.retain_semantic_boundary(authority(), 2048).unwrap()
                else {
                    continue;
                };
                assert_eq!(global.total_work().byte_reads, before.byte_reads);
                assert_eq!(
                    global.total_work().transitions,
                    before.transitions + captured.transitions
                );
                let saved = (
                    global.next_byte(),
                    global.position.byte,
                    global.position.utf16,
                    global.position.line,
                    global.position.previous_cr,
                    global.classified,
                    global.begin_list_item,
                    global.last_line_blank,
                );
                let issued_before = issued.iter().map(signature).collect::<Vec<_>>();
                assert!(matches!(
                    global.retain_semantic_boundary(authority(), 2048),
                    Err(GlobalFrontierError::InvalidFrame)
                ));
                let allocations = budget.payload_counts().1;
                global
                    .rewind_semantic_source_to_start(authority(), 2048)
                    .unwrap();
                assert_eq!(global.next_byte(), 0);
                assert_eq!(global.classified, (0, 0));
                assert_eq!(budget.payload_counts().1, allocations); // 空根只借原预算，不分配或清零历史fuel。
                let probed = tail(&mut global, source, width);
                assert_eq!(probed.iter().map(signature).collect::<Vec<_>>(), expected);
                let looked = global.total_work();
                let allocation_work = budget.payload_counts();
                let restored = global.restore_semantic_boundary(authority(), 2048).unwrap();
                assert_eq!(global.total_work().byte_reads, looked.byte_reads);
                assert_eq!(
                    global.total_work().transitions,
                    looked.transitions + restored.transitions
                );
                assert_eq!(budget.payload_counts().1, allocation_work.1);
                assert_eq!(budget.payload_counts().2, allocation_work.2);
                assert_eq!(
                    saved,
                    (
                        global.next_byte(),
                        global.position.byte,
                        global.position.utf16,
                        global.position.line,
                        global.position.previous_cr,
                        global.classified,
                        global.begin_list_item,
                        global.last_line_blank
                    )
                );
                assert!(!global.semantic_boundary_retained());
                assert_eq!(
                    issued.iter().map(signature).collect::<Vec<_>>(),
                    issued_before
                );
                let replay = tail(&mut global, source, width);
                assert_eq!(
                    replay.iter().map(signature).collect::<Vec<_>>(),
                    expected[issued.len()..]
                );
                drop((global, issued, probed, replay));
                assert_eq!(budget.payload_counts().0, 0); // 最后真实scope结束后，所有COW页归原库存。
            }
        }
    }
}

#[test]
fn reference_tail_and_partial_unicode_are_not_fabricated_block_boundaries() {
    let source = "# before\r\n\r\n[label]: /url \"中文😀\"\r\n\r\n# after\n";
    let mut global = frontier(source);
    while !matches!(
        step(&mut global, source, 1),
        GlobalStep::Fact {
            fact: GlobalFact::LeafBlock(_),
            ..
        }
    ) {}
    global
        .retain_semantic_boundary(authority(), 2048)
        .unwrap()
        .unwrap();
    let saved = global.next_byte();
    global
        .rewind_semantic_source_to_start(authority(), 2048)
        .unwrap();
    while !matches!(
        step(&mut global, source, 1),
        GlobalStep::Fact {
            fact: GlobalFact::Reference(_),
            ..
        }
    ) {}
    // 当前Reference尾尚未闭合，旧真实恢复点仍可取回，但不能用当前位置覆盖它。
    global.restore_semantic_boundary(authority(), 2048).unwrap();
    assert_eq!(global.next_byte(), saved);
    while !matches!(
        step(&mut global, source, 1),
        GlobalStep::Fact {
            fact: GlobalFact::Reference(_),
            ..
        }
    ) {}
    assert!(global
        .retain_semantic_boundary(authority(), 2048)
        .unwrap()
        .is_none());
    while !matches!(
        step(&mut global, source, 1),
        GlobalStep::Fact {
            fact: GlobalFact::LeafBlock(_),
            ..
        }
    ) {}
    global
        .retain_semantic_boundary(authority(), 2048)
        .unwrap()
        .unwrap();
    let saved = global.position;
    global
        .rewind_semantic_source_to_start(authority(), 2048)
        .unwrap();
    while global.position.utf8_left == 0 {
        step(&mut global, source, 1);
    }
    assert!(global.position.utf8_left > 0);
    global.restore_semantic_boundary(authority(), 2048).unwrap();
    assert_eq!(global.position.byte, saved.byte);
    assert_eq!(global.position.utf16, saved.utf16);
    assert_eq!(global.position.utf8_left, saved.utf8_left);
    assert_eq!(global.position.previous_cr, saved.previous_cr);
}

#[test]
fn failure_never_restores_a_cancelled_superseded_or_exhausted_frontier() {
    for cause in [
        GlobalFrontierError::Cancelled,
        GlobalFrontierError::SourceSuperseded,
        GlobalFrontierError::WorkFuel,
    ] {
        let source = "> # first\n\nlast\n";
        let mut global = frontier(source);
        while !matches!(
            step(&mut global, source, 1),
            GlobalStep::Fact {
                fact: GlobalFact::LeafBlock(_),
                ..
            }
        ) {}
        global
            .retain_semantic_boundary(authority(), 2048)
            .unwrap()
            .unwrap();
        global
            .rewind_semantic_source_to_start(authority(), 2048)
            .unwrap();
        let before = global.total_work();
        let mut invalid = authority();
        match cause {
            GlobalFrontierError::Cancelled => invalid.cancelled = true,
            GlobalFrontierError::SourceSuperseded => invalid.current_revision += 1,
            GlobalFrontierError::WorkFuel => {
                global.limits.max_transitions = before.transitions + RESET_WORK - 1
            }
            _ => unreachable!("有限失败样本只覆盖三种真实撤权"),
        }
        assert_eq!(
            global.restore_semantic_boundary(invalid, 2048).unwrap_err(),
            cause
        );
        assert!(!global.semantic_boundary_retained());
        assert_eq!(global.total_work(), before);
        assert_eq!(
            global
                .retain_semantic_boundary(authority(), 2048)
                .unwrap_err(),
            cause
        );
        assert_eq!(
            global
                .rewind_semantic_source_to_start(authority(), 2048)
                .unwrap_err(),
            cause
        );
    }
}

#[test]
fn reference_title_backup_waits_for_the_actual_block_boundary() {
    let source = "# before\r\n\r\n[label]: /url\r\n'not title' junk 中文😀\r\n\r\n# after\n";
    // 同URL行直接追加普通字会使整个定义无效；此处用原Parser真实接受的跨行title失败回退。
    assert!(
        crate::Parser::new_ext(source, Options::ENABLE_TABLES | Options::ENABLE_GFM)
            .reference_definitions()
            .iter()
            .next()
            .is_some()
    );
    let expected = tail(&mut frontier(source), source, 3)
        .iter()
        .map(signature)
        .collect::<Vec<_>>();
    for width in [1, 7] {
        let mut global = frontier(source);
        let mut prefix = Vec::new();
        loop {
            match step(&mut global, source, width) {
                GlobalStep::Fact { fact, .. } => {
                    let reference = matches!(fact, GlobalFact::Reference(_));
                    prefix.push(signature(&fact));
                    if reference {
                        break;
                    }
                }
                GlobalStep::Complete { .. } => panic!("实际引用夹具没有产生Reference Fact"),
                _ => {} // 原同一前沿继续，事实只有闭合后才计入主来源前缀。
            }
        }
        assert!(global
            .retain_semantic_boundary(authority(), 2048)
            .unwrap()
            .is_none());
        // URL后的真实尾行及title回退后的正文必须继续分类，不能把Reference发布点伪签成普通块。
        loop {
            if global
                .retain_semantic_boundary(authority(), 2048)
                .unwrap()
                .is_some()
            {
                break;
            }
            match step(&mut global, source, width) {
                GlobalStep::Fact { fact, .. } => prefix.push(signature(&fact)),
                GlobalStep::Complete { .. } => panic!("真实引用尾应在后继标题前形成可恢复边界"),
                _ => {} // 尚在引用续态时继续真实工作，不重放失败输入或编造根边界。
            }
        }
        let saved = (global.next_byte(), global.position.line, global.classified);
        global
            .rewind_semantic_source_to_start(authority(), 2048)
            .unwrap();
        drop(tail(&mut global, source, width));
        global.restore_semantic_boundary(authority(), 2048).unwrap();
        assert_eq!(
            saved,
            (global.next_byte(), global.position.line, global.classified)
        );
        prefix.extend(tail(&mut global, source, width).iter().map(signature));
        assert_eq!(prefix, expected);
    }
}

#[test]
fn rejected_budget_keeps_the_original_boundary_but_advance_cancellation_revokes_it() {
    let source = "> # first\n\nlast\n";
    let mut global = frontier(source);
    global
        .retain_semantic_boundary(authority(), 2048)
        .unwrap()
        .unwrap();
    let work = global.total_work();
    assert_eq!(
        global
            .restore_semantic_boundary(authority(), 1)
            .unwrap_err(),
        GlobalFrontierError::InvalidBudget
    );
    assert_eq!(global.total_work(), work);
    assert!(global.semantic_boundary_retained());
    // 参数拒绝没有夺走已接纳操作的恢复权；真实取消则同时撤销旧恢复点并成为sticky首因。
    global
        .rewind_semantic_source_to_start(authority(), 2048)
        .unwrap();
    let mut cancelled = authority();
    cancelled.cancelled = true;
    assert!(matches!(
        global.advance(
            SourceFrame {
                source_revision: 71,
                byte_start: 0,
                bytes: &source.as_bytes()[..1],
                real_eof: false
            },
            cancelled,
            4096
        ),
        Err(GlobalFrontierError::Cancelled)
    ));
    assert!(!global.semantic_boundary_retained());
    assert_eq!(
        global
            .restore_semantic_boundary(authority(), 2048)
            .unwrap_err(),
        GlobalFrontierError::Cancelled
    );
}

#[test]
fn restore_preserves_true_completion_without_upgrading_a_short_frame_to_eof() {
    let source = "# title\n";
    let mut global = frontier(source);
    while !matches!(
        step(&mut global, source, 1),
        GlobalStep::Fact {
            fact: GlobalFact::LeafBlock(_),
            ..
        }
    ) {}
    global
        .retain_semantic_boundary(authority(), 2048)
        .unwrap()
        .unwrap();
    assert!(!global.semantic_boundary.as_ref().unwrap().closed);
    global
        .rewind_semantic_source_to_start(authority(), 2048)
        .unwrap();
    drop(tail(&mut global, source, 1));
    global.restore_semantic_boundary(authority(), 2048).unwrap();
    // 前探已经读过真实EOF，主来源此前仍只交付闭合标题；恢复不能偷授主来源完整结束事实。
    assert!(matches!(
        global
            .advance(
                SourceFrame {
                    source_revision: 71,
                    byte_start: source.len(),
                    bytes: &[],
                    real_eof: false
                },
                authority(),
                4096
            )
            .unwrap(),
        GlobalStep::NeedInput { .. }
    ));
    drop(tail(&mut global, source, 1));
    global
        .retain_semantic_boundary(authority(), 2048)
        .unwrap()
        .unwrap();
    assert!(global.semantic_boundary.as_ref().unwrap().closed);
    let before = global.total_work();
    global
        .rewind_semantic_source_to_start(authority(), 2048)
        .unwrap();
    drop(tail(&mut global, source, 1));
    global.restore_semantic_boundary(authority(), 2048).unwrap();
    assert!(matches!(global.phase, Phase::Closed));
    assert!(global.total_work().transitions > before.transitions);
    assert!(matches!(
        step(&mut global, source, 1),
        GlobalStep::Complete { .. }
    ));
}
