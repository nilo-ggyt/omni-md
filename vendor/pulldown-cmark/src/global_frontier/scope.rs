// Copyright 2015 Google Inc. All rights reserved.
// 捕获角色来自固定 pulldown-cmark 0.13.4 的真实结构 spine；完整 MIT 条款见 LICENSE。

//! 只读已封闭块的原 scope capture；不制造最终 Markdown Tag、首行 prefix 或 inline 权限。

use crate::structural_spine::{
    StructuralCursorStep, StructuralKind, StructuralSpineCursor, StructuralSpineSnapshot,
};

use super::{GlobalFrontierError, GlobalSourceAuthority, GlobalWork, VerifiedGlobalInlineBlock};

const MIN_STEP: usize = 2 * 1024;
const MAX_STEP: usize = 64 * 1024;
/// 固定 512 个 page claims 的初始化与迭代释放预留；不是一份新的共享 role fuel。
const CURSOR_RESERVE: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 保留每个实际层级。捕获时 tight 不是未来闭合列表的最终 Tag，marker 也不包含起始编号。
pub enum CapturedScopeRole {
    Quote,
    List {
        marker: u8,
        tight_at_capture: bool,
    },
    Item {
        indent: usize,
    },
    Definition {
        indent: usize,
    },
    /// Some 只来自原 producer 已完成的 label/colon/空白；None 仅表示原角色本身不含 opening 事实。
    /// 没有该事实的 capture 仍可说明四列续行种类，不能授权首行 prefix 的视觉解释。
    Footnote {
        body_start: Option<usize>,
    },
    Paragraph,
    Table,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 构造只属于真实块票据；header 的源锚点与 self 层说明不可代替 Core 的原 DocRoot 身份。
pub struct CapturedScopeHeader {
    source_revision: u64,
    source_end: usize,
    block_source_start: usize,
    depth: usize,
    self_role_depth: Option<usize>,
}

impl CapturedScopeHeader {
    pub fn source_revision(&self) -> u64 {
        self.source_revision
    }
    pub fn source_end(&self) -> usize {
        self.source_end
    }
    pub fn block_source_start(&self) -> usize {
        self.block_source_start
    }
    pub fn depth(&self) -> usize {
        self.depth
    }
    /// InlineBlock 捕获仍含 Paragraph 自身；LeafBlock 捕获只含实际祖先，因此返回 None。
    pub fn self_role_depth(&self) -> Option<usize> {
        self.self_role_depth
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturedScopeStep {
    Progress {
        work: GlobalWork,
    },
    Entry {
        depth_index: usize,
        source_byte_start: usize,
        role: CapturedScopeRole,
        work: GlobalWork,
    },
    Complete {
        work: GlobalWork,
    },
}

/// factory 只克隆原页 claim；固定 page-pointer 工作区在首次 advance 预留收费后才初始化。
/// 不持 source/root/mmap，也不保存已经返回的角色集合；Core 仍须绑定原 DocRoot Weak 与批次收据。
/// 调用方须在保留 cursor 前准入其固定 workspace 与数量；共享页预算不包含调用方容器容量。
pub struct CapturedScopeCursor {
    header: CapturedScopeHeader,
    seed: Option<StructuralSpineSnapshot>,
    cursor: Option<StructuralSpineCursor>,
    complete: bool,
    failure: Option<GlobalFrontierError>,
    allocated: usize,
}

impl std::fmt::Debug for CapturedScopeCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 诊断只显示有限 capture 标量；不沿共享页递归展开祖先或正文。
        formatter
            .debug_struct("CapturedScopeCursor")
            .field("header", &self.header)
            .field("initialized", &self.cursor.is_some())
            .field("complete", &self.complete)
            .field("failure", &self.failure)
            .finish()
    }
}

impl VerifiedGlobalInlineBlock {
    /// 使用原闭合时 capture；Setext 的最终 kind 不会把原 Paragraph 层偷换成 Heading。
    pub fn scope_cursor(&self) -> Result<CapturedScopeCursor, GlobalFrontierError> {
        // 原票据必须含 producer 实际压入的 Paragraph；内部 capture 缺失也只返回错误，不伪造 None。
        let self_role_depth = self
            .spine
            .len()
            .checked_sub(1)
            .ok_or(GlobalFrontierError::InvalidFrame)?;
        CapturedScopeCursor::new(
            self.revision,
            self.source_end,
            self.start,
            Some(self_role_depth),
            &self.spine,
        )
    }
}

impl CapturedScopeCursor {
    /// 只供票据内部 factory 调用；外部不能传任意源坐标、snapshot 或 self 层建立权限。
    pub(super) fn new(
        source_revision: u64,
        source_end: usize,
        block_source_start: usize,
        self_role_depth: Option<usize>,
        spine: &StructuralSpineSnapshot,
    ) -> Result<Self, GlobalFrontierError> {
        if block_source_start > source_end
            || self_role_depth.is_some_and(|depth| depth >= spine.len())
        {
            return Err(GlobalFrontierError::InvalidFrame); // 先拒绝不一致 capture，尚未克隆任何共享页。
        }
        Ok(Self {
            header: CapturedScopeHeader {
                source_revision,
                source_end,
                block_source_start,
                depth: spine.len(),
                self_role_depth,
            },
            seed: Some(spine.clone()),
            cursor: None,
            complete: false,
            failure: None,
            allocated: 0,
        })
    }

    /// 只报告原页游标实际取得的目录累计量；Core 将增量记入同一来源请求，释放不重发额度。
    pub fn allocated_workspace_bytes(&self) -> usize {
        self.allocated
    }

    pub fn header(&self) -> &CapturedScopeHeader {
        &self.header
    }

    fn fail(&mut self, error: GlobalFrontierError) -> GlobalFrontierError {
        // 所有退出共享同一 sticky 首因；同时释放待初始化或已遍历的纯 scope claims。
        self.failure = Some(error);
        self.seed = None;
        self.cursor = None;
        error
    }

    /// 每次最多交付一个角色；page Building、Entry 与终止各有真实工作收费，不 flatten 深 scope。
    pub fn advance(
        &mut self,
        authority: GlobalSourceAuthority,
        budget: usize,
    ) -> Result<CapturedScopeStep, GlobalFrontierError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        // 真实 owner 撤销优先于参数检查；旧 revision 恢复或再给预算不能复活已取消的 capture。
        if authority.cancelled {
            return Err(self.fail(GlobalFrontierError::Cancelled));
        }
        if authority.current_revision != self.header.source_revision {
            return Err(self.fail(GlobalFrontierError::SourceSuperseded));
        }
        if !(MIN_STEP..=MAX_STEP).contains(&budget) {
            return Err(GlobalFrontierError::InvalidBudget); // 协议错误未推进状态，可用合法预算重试。
        }
        if self.complete {
            return Ok(CapturedScopeStep::Complete {
                work: GlobalWork::default(),
            });
        }
        let mut work = GlobalWork {
            byte_reads: 0,
            transitions: CURSOR_RESERVE,
        };
        if self.cursor.is_none() {
            // 先承诺本 step 固定 workspace 收费，再初始化最多 512 槽；仍共享原 producer 的页与 fuel。
            let seed = self
                .seed
                .take()
                .ok_or_else(|| self.fail(GlobalFrontierError::InvalidFrame))?;
            self.cursor = Some(seed.cursor());
        }
        while work.transitions < budget {
            work.transitions += 1;
            let cursor = self.cursor.as_mut().expect("初始化后拥有原 scope 游标");
            let before = cursor.allocated_workspace_bytes();
            let step = cursor.next();
            self.allocated += cursor.allocated_workspace_bytes() - before;
            let step = match step {
                Ok(step) => step,
                Err(error) => return Err(self.fail(GlobalFrontierError::Spine(error))),
            };
            match step {
                StructuralCursorStep::Building => (), // 每次只移交一页，原共享 role fuel 也在 next 内消费。
                StructuralCursorStep::Entry { index, entry } => {
                    if index >= self.header.depth || entry.source_start > self.header.source_end {
                        return Err(self.fail(GlobalFrontierError::InvalidFrame));
                    }
                    // 每个原角色独立映射；没有丢层、猜编号、替换最终 tight 或重造 Quote 标签。
                    let role = match entry.kind {
                        StructuralKind::Quote => CapturedScopeRole::Quote,
                        StructuralKind::List { marker, tight } => CapturedScopeRole::List {
                            marker,
                            tight_at_capture: tight,
                        },
                        StructuralKind::Item { indent } => CapturedScopeRole::Item { indent },
                        StructuralKind::Definition { indent } => {
                            CapturedScopeRole::Definition { indent }
                        }
                        StructuralKind::Footnote { body_start } => {
                            let body_start = body_start.map(std::num::NonZeroUsize::get);
                            if body_start.is_some_and(|end| {
                                end <= entry.source_start || end > self.header.block_source_start
                            }) {
                                return Err(self.fail(GlobalFrontierError::InvalidFrame));
                            }
                            CapturedScopeRole::Footnote { body_start }
                        }
                        StructuralKind::Paragraph => CapturedScopeRole::Paragraph,
                        StructuralKind::Table => CapturedScopeRole::Table,
                        StructuralKind::Other => CapturedScopeRole::Other,
                    };
                    return Ok(CapturedScopeStep::Entry {
                        depth_index: index,
                        source_byte_start: entry.source_start,
                        role,
                        work,
                    });
                }
                StructuralCursorStep::End => {
                    // 固定预留覆盖至多 512 个 Arc claim 的迭代释放；完成后不再保留任何 scope payload。
                    self.cursor = None;
                    self.complete = true;
                    return Ok(CapturedScopeStep::Complete { work });
                }
            }
        }
        Ok(CapturedScopeStep::Progress { work })
    }
}

#[cfg(test)]
mod tests;
