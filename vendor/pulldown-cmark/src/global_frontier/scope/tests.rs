// Copyright 2015 Google Inc. All rights reserved.
// 合成 scope 验证保留固定 0.13.4 的捕获角色；完整 MIT 条款见 LICENSE。

use super::*;
use crate::{
    block_frontier::SourceFrame,
    global_frontier::{
        GlobalFact, GlobalFrontier, GlobalFrontierLimits, GlobalStep, InlineBlockKind,
        StructuralSpineError, StructuralSpineLimits, VerifiedGlobalLeafBlock,
    },
    structural_spine::StructuralEntry,
    HeadingLevel, Options,
};

fn limits(role_work: usize) -> StructuralSpineLimits {
    StructuralSpineLimits {
        max_depth: 16_384,
        max_live_page_bytes: 4 * 1024 * 1024,
        max_page_allocations: 1_000_000,
        max_role_work: role_work,
    }
}

fn authority() -> GlobalSourceAuthority {
    GlobalSourceAuthority {
        current_revision: 53,
        cancelled: false,
    }
}

fn private_inline_fixture(kinds: &[StructuralKind], role_work: usize) -> VerifiedGlobalInlineBlock {
    let mut spine = StructuralSpineSnapshot::new(limits(role_work)).unwrap();
    for (index, &kind) in kinds.iter().enumerate() {
        spine
            .push(StructuralEntry {
                kind,
                source_start: index,
            })
            .unwrap();
    }
    // 私有 fixture 只测结构投影，不冒充真实 parser 输入；最后 Paragraph 对应 factory 的 self 合同。
    assert_eq!(kinds.last(), Some(&StructuralKind::Paragraph));
    VerifiedGlobalInlineBlock {
        revision: 53,
        source_end: 1_000_000,
        options: Options::empty(),
        kind: InlineBlockKind::Paragraph,
        start: kinds.len() - 1,
        physical_start: 0,
        // 仅补齐既有 scope 私有结构夹具；origin producer 的测试仍须来自真实 Global 票据。
        first_state_origin: 0,
        first_state: crate::line_start_stream::LineStartState {
            ix: kinds.len() - 1,
            ..Default::default()
        },
        end: kinds.len(),
        lexical_end: kinds.len(),
        last_content_end: kinds.len(),
        last_line_end: kinds.len(),
        spine,
    }
}

fn collect(cursor: &mut CapturedScopeCursor) -> Vec<(usize, usize, CapturedScopeRole)> {
    let mut result = Vec::new();
    for _ in 0..cursor.header().depth() + 1024 {
        let step = cursor.advance(authority(), MIN_STEP).unwrap();
        let work = match step {
            CapturedScopeStep::Progress { work }
            | CapturedScopeStep::Entry { work, .. }
            | CapturedScopeStep::Complete { work } => work,
        };
        assert_eq!(work.byte_reads, 0);
        assert!(work.transitions <= MIN_STEP);
        match step {
            CapturedScopeStep::Entry {
                depth_index,
                source_byte_start,
                role,
                ..
            } => result.push((depth_index, source_byte_start, role)),
            CapturedScopeStep::Complete { .. } => {
                assert!(
                    matches!(cursor.advance(authority(), MIN_STEP).unwrap(), CapturedScopeStep::Complete { work } if work == GlobalWork::default())
                );
                return result;
            }
            CapturedScopeStep::Progress { .. } => (), // 固定工作区准备必须由后继合法 step 继续。
        }
    }
    panic!("原共享 scope 没有在有限步内结束");
}

fn real_blocks(source: &str) -> (Vec<VerifiedGlobalInlineBlock>, Vec<VerifiedGlobalLeafBlock>) {
    let mut global = GlobalFrontier::new(
        53,
        source.len(),
        Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_GFM
            | Options::ENABLE_FOOTNOTES,
        GlobalFrontierLimits {
            spine: limits(10_000_000),
            max_byte_work: 10_000_000,
            max_transitions: 10_000_000,
        },
    )
    .unwrap();
    let mut inline = Vec::new();
    let mut leaves = Vec::new();
    for _ in 0..source.len() * 100 + 10_000 {
        let start = global.next_byte();
        let end = (start + 3).min(source.len());
        match global
            .advance(
                SourceFrame {
                    source_revision: 53,
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
            } => inline.push(block),
            GlobalStep::Fact {
                fact: GlobalFact::LeafBlock(block),
                ..
            } => leaves.push(block),
            GlobalStep::Complete { .. } => return (inline, leaves),
            _ => (), // 其它真实 producer 事实保留原路径，不从数量推断当前 scope。
        }
    }
    panic!("真实合成块没有完成");
}

#[test]
fn private_scope_fixture_preserves_every_role_and_original_depth_without_tag_invention() {
    let kinds = [
        StructuralKind::Quote,
        StructuralKind::List {
            marker: b'.',
            tight: false,
        },
        StructuralKind::List {
            marker: b'*',
            tight: true,
        },
        StructuralKind::Item { indent: 7 },
        StructuralKind::Definition { indent: 3 },
        StructuralKind::Footnote,
        StructuralKind::Paragraph,
        StructuralKind::Table,
        StructuralKind::Other,
        StructuralKind::Paragraph,
    ];
    let block = private_inline_fixture(&kinds, 1_000_000);
    let mut cursor = block.scope_cursor().unwrap();
    assert_eq!(cursor.header().source_revision(), 53);
    assert_eq!(cursor.header().source_end(), 1_000_000);
    assert_eq!(cursor.header().block_source_start(), kinds.len() - 1);
    assert_eq!(cursor.header().self_role_depth(), Some(kinds.len() - 1));
    let expected = [
        CapturedScopeRole::Quote,
        CapturedScopeRole::List {
            marker: b'.',
            tight_at_capture: false,
        },
        CapturedScopeRole::List {
            marker: b'*',
            tight_at_capture: true,
        },
        CapturedScopeRole::Item { indent: 7 },
        CapturedScopeRole::Definition { indent: 3 },
        CapturedScopeRole::Footnote,
        CapturedScopeRole::Paragraph,
        CapturedScopeRole::Table,
        CapturedScopeRole::Other,
        CapturedScopeRole::Paragraph,
    ];
    assert_eq!(
        collect(&mut cursor),
        expected
            .into_iter()
            .enumerate()
            .map(|(index, role)| (index, index, role))
            .collect::<Vec<_>>()
    );
}

#[test]
fn real_inline_and_setext_keep_the_original_paragraph_self_capture() {
    for (source, kind) in [
        ("> 001. [x] 标题\n>      tail\n", InlineBlockKind::Paragraph),
        (
            "> - 标题\n>   ---\n",
            InlineBlockKind::SetextHeading(HeadingLevel::H2),
        ),
    ] {
        let (blocks, _) = real_blocks(source);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.kind(), kind);
        let before = (
            block.source_range(),
            block.lexical_range(),
            block.terminal_eol_range(),
            block.scope_depth(),
        );
        let mut cursor = block.scope_cursor().unwrap();
        assert_eq!(cursor.header().self_role_depth(), Some(3));
        let roles = collect(&mut cursor);
        assert_eq!(roles.len(), 4);
        assert_eq!(roles[0].2, CapturedScopeRole::Quote);
        assert!(matches!(roles[1].2, CapturedScopeRole::List { .. }));
        assert!(matches!(roles[2].2, CapturedScopeRole::Item { .. }));
        assert_eq!(
            roles[3],
            (3, block.source_range().start, CapturedScopeRole::Paragraph)
        );
        assert_eq!(
            before,
            (
                block.source_range(),
                block.lexical_range(),
                block.terminal_eol_range(),
                block.scope_depth()
            )
        );
    }
    // Footnote 会在同一物理行重置 origin；本片只返回真实捕获锚点，不重猜首行 prefix。
    let (blocks, _) = real_blocks("[^f]: footnote\n    tail\n");
    assert_eq!(blocks.len(), 1);
    let mut cursor = blocks[0].scope_cursor().unwrap();
    assert_eq!(cursor.header().self_role_depth(), Some(1));
    assert_eq!(
        collect(&mut cursor),
        vec![
            (0, 0, CapturedScopeRole::Footnote),
            (
                1,
                blocks[0].source_range().start,
                CapturedScopeRole::Paragraph
            )
        ]
    );
}

#[test]
fn real_leaf_scope_contains_only_ancestors_and_keeps_empty_root_capture() {
    let (_, leaves) = real_blocks("> - ``` rust\n>   body\n>   ```\n");
    assert_eq!(leaves.len(), 1);
    let block = &leaves[0];
    let before = (
        block.source_range(),
        block.consumed_end(),
        block.physical_start(),
        block.scope_depth(),
    );
    let mut cursor = block.scope_cursor().unwrap();
    assert_eq!(cursor.header().self_role_depth(), None);
    assert_eq!(
        cursor.header().block_source_start(),
        block.source_range().start
    );
    let roles = collect(&mut cursor);
    assert_eq!(
        roles.iter().map(|entry| entry.2).collect::<Vec<_>>(),
        [
            CapturedScopeRole::Quote,
            CapturedScopeRole::List {
                marker: b'-',
                tight_at_capture: true
            },
            CapturedScopeRole::Item { indent: 2 }
        ]
    );
    assert_eq!(
        before,
        (
            block.source_range(),
            block.consumed_end(),
            block.physical_start(),
            block.scope_depth()
        )
    );
    let (_, root) = real_blocks("# 根标题\n");
    let mut cursor = root[0].scope_cursor().unwrap();
    assert_eq!(cursor.header().depth(), 0);
    assert_eq!(cursor.header().self_role_depth(), None);
    assert!(collect(&mut cursor).is_empty());
}

fn observer_without_pages(block: &VerifiedGlobalInlineBlock) -> StructuralSpineSnapshot {
    let mut observer = block.spine.clone();
    while !observer.is_empty() {
        observer.pop().unwrap();
    }
    observer
}

#[test]
fn deep_cursor_uses_original_shared_pages_and_resumes_with_the_minimum_budget() {
    let mut kinds = vec![StructuralKind::Quote; 2048];
    kinds.push(StructuralKind::Paragraph);
    let block = private_inline_fixture(&kinds, 1_000_000);
    let observer = observer_without_pages(&block);
    let counts_before = observer.payload_counts();
    let mut cursor = block.scope_cursor().unwrap();
    assert!(cursor.seed.as_ref().unwrap().shares_tail(&block.spine));
    assert!(cursor.cursor.is_none());
    assert_eq!(observer.payload_counts(), counts_before);
    assert_eq!(
        cursor.advance(authority(), MIN_STEP - 1).unwrap_err(),
        GlobalFrontierError::InvalidBudget
    );
    assert_eq!(observer.payload_counts(), counts_before);
    drop(block);
    assert!(observer.payload_counts().0 > 0);
    let entries = collect(&mut cursor);
    assert_eq!(entries.len(), kinds.len());
    assert_eq!(entries.first(), Some(&(0, 0, CapturedScopeRole::Quote)));
    assert_eq!(
        entries.last(),
        Some(&(
            kinds.len() - 1,
            kinds.len() - 1,
            CapturedScopeRole::Paragraph
        ))
    );
    assert_eq!(observer.payload_counts().0, 0);
    assert_eq!(observer.payload_counts().1, counts_before.1); // 遍历没有分配新页或刷新原 allocation fuel。
}

#[test]
fn cancellation_and_supersession_are_sticky_and_drop_pending_or_initialized_claims() {
    for initialize in [false, true] {
        for cancelled in [false, true] {
            let mut kinds = vec![StructuralKind::Quote; 2048];
            kinds.push(StructuralKind::Paragraph);
            let block = private_inline_fixture(&kinds, 1_000_000);
            let observer = observer_without_pages(&block);
            let mut cursor = block.scope_cursor().unwrap();
            drop(block);
            if initialize {
                assert!(matches!(
                    cursor.advance(authority(), MIN_STEP).unwrap(),
                    CapturedScopeStep::Entry { .. }
                ));
            }
            let revoked = GlobalSourceAuthority {
                current_revision: if cancelled { 53 } else { 54 },
                cancelled,
            };
            let expected = if cancelled {
                GlobalFrontierError::Cancelled
            } else {
                GlobalFrontierError::SourceSuperseded
            };
            assert_eq!(cursor.advance(revoked, MIN_STEP).unwrap_err(), expected);
            assert_eq!(observer.payload_counts().0, 0);
            assert!(cursor.seed.is_none() && cursor.cursor.is_none());
            assert_eq!(cursor.advance(authority(), MIN_STEP).unwrap_err(), expected);
            assert_eq!(observer.payload_counts().0, 0);
        }
    }
}

#[test]
fn shared_role_fuel_exhaustion_releases_pages_without_replenishing_the_capture() {
    let mut kinds = vec![StructuralKind::Quote; 512];
    kinds.push(StructuralKind::Paragraph);
    // push 和无页 observer 的逐层 pop 各用 depth 次；只再给两页 Building 的原共享 fuel。
    let block = private_inline_fixture(&kinds, kinds.len() * 2 + 2);
    let observer = observer_without_pages(&block);
    let mut cursor = block.scope_cursor().unwrap();
    drop(block);
    let expected = GlobalFrontierError::Spine(StructuralSpineError::RoleWorkFuel);
    assert_eq!(cursor.advance(authority(), MIN_STEP).unwrap_err(), expected);
    assert_eq!(observer.payload_counts().0, 0);
    assert!(cursor.seed.is_none() && cursor.cursor.is_none());
    assert_eq!(cursor.advance(authority(), MAX_STEP).unwrap_err(), expected);
    assert_eq!(observer.payload_counts().0, 0);
}
