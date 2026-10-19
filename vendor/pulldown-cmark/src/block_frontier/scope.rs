// Copyright 2015 Google Inc. All rights reserved.
// 本地有限scope桥复用固定0.13.4原结构角色；完整MIT条款见LICENSE。

//! 只读真实table checkpoint的共享容器scope，不复制角色Vec或猜测prefix含义。

use super::TableCheckpoint;
use crate::{
    global_frontier::{GlobalFrontierError, GlobalSourceAuthority},
    structural_spine::{StructuralCursorStep, StructuralKind, StructuralSpineCursor},
};

const MIN_STEP: usize = 2 * 1_024;
const MAX_STEP: usize = 64 * 1_024;
// 固定cursor最多512个页claim，收尾以迭代Arc drop释放；不按未受限depth递归。
const CURSOR_WORK: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 只公开当前presentation所需的祖先角色。Other仍占原depth位置，不压缩原spine。
pub enum TableScopeKind {
    BlockQuote,
    OrderedList,
    UnorderedList,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// work是本次固定收尾预留加实际page/entry转换；没有源字节读取或正文构造。
pub enum TableScopeStep {
    Progress {
        work: usize,
    },
    Entry {
        depth_index: usize,
        kind: TableScopeKind,
        source_byte_start: usize,
        work: usize,
    },
    Complete {
        work: usize,
    },
}

/// 单个cursor只有固定≤4KiB级page-pointer工作区及源标量；所有页仍共享原producer总预算。
/// 持有它不持source/root/mmap；消费者必须继续核创建checkpoint时的DocRoot弱身份/代次。
pub struct TableScopeCursor {
    source_revision: u64,
    source_end: usize,
    header_byte_start: usize,
    depth: usize,
    cursor: Option<StructuralSpineCursor>,
    failure: Option<GlobalFrontierError>,
}

impl std::fmt::Debug for TableScopeCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不递归输出共享页或正文；默认诊断只保有限权限与深度标量。
        formatter
            .debug_struct("TableScopeCursor")
            .field("source_revision", &self.source_revision)
            .field("header_byte_start", &self.header_byte_start)
            .field("depth", &self.depth)
            .field("complete", &self.cursor.is_none())
            .field("failure", &self.failure)
            .finish()
    }
}

impl TableCheckpoint {
    /// 构造只初始化固定page-pointer槽，不同步遍历祖先；调用者为这一步计固定1024转换预留。
    /// 每个后继advance才逐页准备root→leaf顺序；没有新的容量/fuel授予或任意public种子。
    pub fn scope_cursor(&self) -> TableScopeCursor {
        TableScopeCursor {
            source_revision: self.scalars.source_revision,
            source_end: self.scalars.source_end,
            header_byte_start: self.scalars.header_byte_start,
            depth: self.spine.len(),
            cursor: Some(self.spine.cursor()),
            failure: None,
        }
    }
}

impl TableScopeCursor {
    pub fn source_revision(&self) -> u64 {
        self.source_revision
    }
    pub fn header_byte_start(&self) -> usize {
        self.header_byte_start
    }
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// 每step先核owner标量；实际root绑定由Core持有，不能以同revision混用两个文档。
    /// 页面Building与角色Entry各收费一次，首Entry即返回，不在一个回调内flatten任意深度。
    pub fn advance(
        &mut self,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<TableScopeStep, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error); // 失败scope不因恢复旧代次或再给fuel复活。
        }
        let authority_error = if authority.cancelled {
            Some(GlobalFrontierError::Cancelled)
        } else if authority.current_revision != self.source_revision {
            Some(GlobalFrontierError::SourceSuperseded)
        } else {
            None
        };
        if let Some(error) = authority_error {
            self.failure = Some(error);
            self.cursor = None; // 取消同时释放所有纯scope页claims，无后台源owner或新队列。
            return Err(error);
        }
        if !(MIN_STEP..=MAX_STEP).contains(&budget) {
            // 错误调用协议在前置检查拒绝，尚未推进cursor，可重新给合法budget。
            return Err(GlobalFrontierError::InvalidBudget);
        }
        let Some(cursor) = self.cursor.as_mut() else {
            return Ok(TableScopeStep::Complete { work: 0 });
        };
        let mut work = CURSOR_WORK;
        while work < budget {
            work += 1;
            let step = match cursor.next() {
                Ok(step) => step,
                Err(error) => {
                    let error = GlobalFrontierError::Spine(error);
                    self.failure = Some(error);
                    self.cursor = None;
                    return Err(error); // 原共享总fuel失败，不能交付截断的成功scope。
                }
            };
            match step {
                StructuralCursorStep::Building => (), // 每页收费后继续，页数受原512槽硬界限约束。
                StructuralCursorStep::Entry { index, entry } => {
                    if entry.source_start > self.source_end {
                        self.failure = Some(GlobalFrontierError::InvalidFrame);
                        self.cursor = None;
                        return Err(GlobalFrontierError::InvalidFrame);
                    }
                    let kind = match entry.kind {
                        StructuralKind::Quote => TableScopeKind::BlockQuote,
                        StructuralKind::List {
                            marker: b'.' | b')',
                            ..
                        } => TableScopeKind::OrderedList,
                        StructuralKind::List { .. } => TableScopeKind::UnorderedList,
                        _ => TableScopeKind::Other, // item/footnote/table等仍保留其原层号与source锚点。
                    };
                    return Ok(TableScopeStep::Entry {
                        depth_index: index,
                        kind,
                        source_byte_start: entry.source_start,
                        work,
                    });
                }
                StructuralCursorStep::End => {
                    // 固定收尾预算覆盖最多512个页claim的迭代drop；Complete不继续保留scope缓存。
                    self.cursor = None;
                    return Ok(TableScopeStep::Complete { work });
                }
            }
        }
        Ok(TableScopeStep::Progress { work })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        block_frontier::SourceFrame,
        global_frontier::{
            GlobalFact, GlobalFrontier, GlobalFrontierLimits, GlobalStep, StructuralSpineLimits,
        },
        Event, Options, Parser, Tag,
    };

    fn checkpoint(source: &str) -> TableCheckpoint {
        let mut global = GlobalFrontier::new(
            31,
            source.len(),
            Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES | Options::ENABLE_GFM,
            GlobalFrontierLimits {
                spine: StructuralSpineLimits {
                    max_depth: 16_384,
                    max_live_page_bytes: 4 * 1024 * 1024,
                    max_page_allocations: 1_000_000,
                    max_role_work: 200_000_000,
                },
                max_byte_work: source.len() * 1000 + 1_000_000,
                max_transitions: source.len() * 10000 + 1_000_000,
            },
        )
        .unwrap();
        for _ in 0..source.len() * 100 + 10000 {
            let at = global.next_byte();
            let end = (at + 17).min(source.len());
            let step = global
                .advance(
                    SourceFrame {
                        source_revision: 31,
                        byte_start: at,
                        bytes: &source.as_bytes()[at..end],
                        real_eof: end == source.len(),
                    },
                    GlobalSourceAuthority {
                        current_revision: 31,
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
                return global.table_checkpoint().unwrap();
            }
        }
        panic!("真实全局producer未产生table checkpoint");
    }

    fn inherited(source: &str) -> Vec<(TableScopeKind, usize)> {
        let mut expected = Vec::new();
        for (event, range) in Parser::new_ext(
            source,
            Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES | Options::ENABLE_GFM,
        )
        .into_offset_iter()
        {
            match event {
                Event::Start(Tag::BlockQuote(_)) => {
                    expected.push((TableScopeKind::BlockQuote, range.start))
                }
                Event::Start(Tag::List(Some(_))) => {
                    expected.push((TableScopeKind::OrderedList, range.start))
                }
                Event::Start(Tag::List(None)) => {
                    expected.push((TableScopeKind::UnorderedList, range.start))
                }
                Event::Start(Tag::Table(_)) => return expected,
                _ => (),
            }
        }
        panic!("oracle没有table");
    }

    #[test]
    fn real_checkpoint_scope_preserves_parser_ancestors_order_and_source_anchors() {
        for source in [
            "> - | H |\n>   | - |\n>   | 中 |\n",
            "- > | H |\n  > | - |\n  > | body |\n",
            "3. | H |\n   | - |\n   | body |\n",
            "4) | H |\n   | - |\n   | body |\n",
            "+ | H |\n  | - |\n  | body |\n",
        ] {
            let point = checkpoint(source);
            let mut cursor = point.scope_cursor();
            let mut actual = Vec::new();
            let mut entries = 0;
            loop {
                match cursor
                    .advance(
                        GlobalSourceAuthority {
                            current_revision: 31,
                            cancelled: false,
                        },
                        2048,
                    )
                    .unwrap()
                {
                    TableScopeStep::Entry {
                        depth_index,
                        kind,
                        source_byte_start,
                        work,
                    } => {
                        assert_eq!(depth_index, entries);
                        entries += 1;
                        assert!(work <= 2048);
                        if kind != TableScopeKind::Other {
                            actual.push((kind, source_byte_start));
                        }
                    }
                    TableScopeStep::Progress { work } => assert!(work > 0 && work <= 2048),
                    TableScopeStep::Complete { work } => {
                        assert!(work <= 2048);
                        break;
                    }
                }
            }
            assert_eq!(entries, cursor.depth());
            assert_eq!(actual, inherited(source), "{source:?}");
            assert!(cursor.cursor.is_none());
            assert_eq!(cursor.source_revision(), 31);
            assert_eq!(cursor.header_byte_start(), 0);
        }
    }

    #[test]
    fn deep_scope_has_fixed_workspace_shared_pages_and_sticky_owner_failure() {
        let prefix = "> ".repeat(2048);
        let source = format!("{prefix}| H |\n{prefix}| - |\n{prefix}| row |\n");
        let point = checkpoint(&source);
        let mut cursor = point.scope_cursor();
        assert_eq!(cursor.depth(), 2049);
        assert!(std::mem::size_of::<TableScopeCursor>() <= 4 * 1024 + 128);
        let mut count = 0;
        loop {
            match cursor
                .advance(
                    GlobalSourceAuthority {
                        current_revision: 31,
                        cancelled: false,
                    },
                    2048,
                )
                .unwrap()
            {
                TableScopeStep::Entry {
                    depth_index, work, ..
                } => {
                    assert_eq!(depth_index, count);
                    count += 1;
                    assert!(work <= 2048);
                }
                TableScopeStep::Progress { work } => assert!(work <= 2048),
                TableScopeStep::Complete { .. } => break,
            }
        }
        assert_eq!(count, 2049);
        for invalid in [
            GlobalSourceAuthority {
                current_revision: 32,
                cancelled: false,
            },
            GlobalSourceAuthority {
                current_revision: 31,
                cancelled: true,
            },
        ] {
            let mut cursor = point.scope_cursor();
            let error = cursor.advance(invalid, 2048).unwrap_err();
            assert!(cursor.cursor.is_none());
            assert_eq!(
                cursor
                    .advance(
                        GlobalSourceAuthority {
                            current_revision: 31,
                            cancelled: false
                        },
                        2048
                    )
                    .unwrap_err(),
                error
            );
        }
    }
}
